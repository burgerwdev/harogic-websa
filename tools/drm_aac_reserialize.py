#!/usr/bin/env python3
"""Parse a raw MPEG-4 AAC-LC access unit (ONLY_LONG window) and dump its scale
factors and spectral codewords — the input to the DRM AAC re-serialisation
(VCB11 + HCR). See docs/en/DRM_AAC_RESERIALIZATION.md.

Status: parses the frame header, section data, scale factors and spectral data for
the ONLY_LONG (window_sequence == 0) case, which is what a stationary signal (and
the fdK-AAC encoder) produces. LONG_START/SHORT/LONG_STOP section-data handling
and the VCB11/HCR re-serialisation are still TODO.
"""
from __future__ import annotations

import sys

from aac_huff_tables import CODEBOOKS
from aac_sfb_offsets import SFB_OFFSETS

SCL = CODEBOOKS["SCL"][0]

# Per-codebook maximum Huffman codeword length (fdK-AAC aac_rom.cpp `aMaxCwLen`).
AMAX_CW_LEN = [0, 11, 9, 20, 16, 13, 11, 14, 12, 17, 14, 49, 0, 0, 0, 0,
               14, 17, 21, 21, 25, 25, 29, 29, 29, 29, 33, 33, 33, 37, 37, 41]


class BitReader:
    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0

    def read(self, n: int) -> int:
        v = 0
        for _ in range(n):
            v = (v << 1) | ((self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1)
            self.pos += 1
        return v

    def bit_at(self, i: int) -> int:
        return (self.data[i >> 3] >> (7 - (i & 7))) & 1

    def bits(self, start: int, n: int) -> str:
        return "".join(str(self.bit_at(start + i)) for i in range(n))


def decode_huffman_word(br: BitReader, codebook) -> int:
    """Inverse of fdK-AAC's CBlock_DecodeHuffmanWordCB (quad-tree, 2 bits/level)."""
    index = 0
    while True:
        index = codebook[index][br.read(2)]
        if index & 1:
            break
        index >>= 2
    if index & 2:
        br.pos -= 1
    return index >> 2


def skip_fil_elements(br: BitReader) -> None:
    """Skip leading FIL (fill) elements (id_syn_ele == 6); some encoders (ffmpeg)
    prepend one."""
    while True:
        ide = br.read(3)
        if ide == 6:
            cnt = br.read(4)
            if cnt == 15:
                cnt += br.read(8) - 1
            br.pos += cnt * 8
            continue
        br.pos -= 3
        break


def parse_frame(data: bytes) -> dict:
    """Parse an ONLY_LONG AAC-LC single-channel frame."""
    br = BitReader(data)
    out = {"id_syn_ele": br.read(3), "element_tag": br.read(4)}
    out["global_gain"] = br.read(8)
    br.read(1)  # ics_reserved_bit
    window_sequence = br.read(2)
    out["window_sequence"] = window_sequence
    out["window_shape"] = br.read(1)
    assert window_sequence == 0, "only ONLY_LONG is implemented"
    out["max_sfb"] = br.read(6)
    br.read(1)  # predictor_data_present
    out["section_data_start"] = br.pos

    # Section data: codebook (4 bits) + length (5 bits, escape 31 = "add 31, more").
    sections = []
    sfb = 0
    while sfb < out["max_sfb"]:
        sect_cb = br.read(4)
        sect_len = 0
        while True:
            incr = br.read(5)
            sect_len += incr
            if incr != 31:
                break
        sections.append((sect_cb, sect_len))
        sfb += sect_len
    out["sections"] = sections
    out["bit_pos_after_sections"] = br.pos

    # Scale factors (SCL Huffman, delta-coded; zero codebook -> sf 0).
    codebooks = []
    for cb, length in sections:
        codebooks += [cb] * length
    codebooks = codebooks[: out["max_sfb"]]
    factor = out["global_gain"]
    sf = []
    for band in range(out["max_sfb"]):
        cb = codebooks[band]
        if cb == 0:
            sf.append(0)
        else:
            factor += decode_huffman_word(br, SCL) - 60
            sf.append(factor - 100)
    out["scale_factors"] = sf
    out["bit_pos_after_scale_factors"] = br.pos

    # Spectral data (per band, using the 24 kHz long-block offsets).
    offsets = SFB_OFFSETS["sfb_24_1024"]
    out["codewords"] = []  # (codebook, codeword_bits, sign_bits) for HCR
    for band in range(out["max_sfb"]):
        cb = codebooks[band]
        if cb == 0 or cb == 13:
            continue
        if cb in (14, 15):
            continue
        table, dim, bits, offset = CODEBOOKS[str(cb)]
        mask = (1 << bits) - 1
        width = offsets[band + 1] - offsets[band]
        for _ in range(width // dim):
            cw_start = br.pos
            idx = decode_huffman_word(br, table)
            cw_len = br.pos - cw_start
            cw_bits = br.bits(cw_start, cw_len)
            sign_bits = ""
            for _ in range(dim):
                coef = (idx & mask) - offset
                idx >>= bits
                if offset == 0 and coef != 0:
                    sign_bits += "1" if br.read(1) else "0"
            if cb == 11:
                for _ in range(2):
                    _read_escape(br)
            out["codewords"].append((cb, cw_bits, sign_bits))
    out["bit_pos_after_spectral"] = br.pos
    return out


def _read_escape(br: BitReader) -> None:
    # Escape value: while the first bit is set, keep reading 4-bit groups.
    while br.read(1):
        br.read(4)


def encode_sections_vcb11(sections: list) -> str:
    """Re-encode the section data as VCB11: 5-bit codebooks (instead of 4) with
    the same 5-bit section lengths (escape 31)."""
    bits = ""
    for cb, length in sections:
        bits += f"{cb:05b}"
        remaining = length
        while remaining >= 31:
            bits += f"{31:05b}"
            remaining -= 31
        bits += f"{remaining:05b}"
    return bits


def crc8(bits: str) -> int:
    """DRM CRC-8 (poly 0x1D, init 0xFF, MSB-first, ones' complement)."""
    reg = 0xFF
    for ch in bits:
        top = (reg >> 7) & 1
        reg = (reg << 1) & 0xFF
        if top ^ (1 if ch == "1" else 0):
            reg ^= 0x1D
    return (~reg) & 0xFF


def reserialize_drm(source: bytes, out: dict) -> str:
    """Assemble the complete DRM AAC frame: the aac_crc_bits byte (CRC-8 over the
    side info), then header + VCB11 section data + the verbatim scale factors +
    the HCR side-info + the reordered spectral data."""
    br = BitReader(source)
    header = br.bits(0, out["section_data_start"])          # header + ICS (verbatim)
    sections = encode_sections_vcb11(out["sections"])       # VCB11 section data
    sf = br.bits(out["bit_pos_after_sections"],             # scale factors (verbatim)
                 out["bit_pos_after_scale_factors"] - out["bit_pos_after_sections"])
    reordered, lrsd, llc = hcr_reorder(out["codewords"])    # HCR reordered spectral
    hcr_side = f"{lrsd:014b}{llc:06b}"                       # HCR side-info
    # aac_crc_bits: CRC-8 over the side info (section data + scale factors + HCR
    # side-info) — the range the TT_DRM decoder checks before the spectral data.
    crc = crc8(sections + sf + hcr_side)
    return f"{crc:08b}" + header + sections + sf + hcr_side + reordered


def hcr_reorder(codewords: list) -> tuple:
    """HCR reordering: sort codewords by codebook, pad each to
    min(AMAX_CW_LEN[cb], longest) bits, then append the sign bits in the same
    sorted order. Returns (bit_string, reordered_length, longest_codeword_length)."""
    ordered = sorted(codewords, key=lambda cw: cw[0])
    longest = max((len(b) for _, b, _ in ordered), default=0)
    bits = ""
    for cb, cw_bits, _sign in ordered:
        width = min(AMAX_CW_LEN[cb], longest)
        bits += cw_bits + "0" * (width - len(cw_bits))  # left-aligned, padded
    for cb, _cw, sign in ordered:
        bits += sign
    return bits, len(bits), longest


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/raw.aac"
    data = open(path, "rb").read()
    br = BitReader(data)
    skip_fil_elements(br)
    frame = data[br.pos >> 3 :]
    print(f"frame starts at byte {br.pos >> 3}, {len(frame)} bytes")
    out = parse_frame(frame)
    print("window_sequence", out["window_sequence"], "max_sfb", out["max_sfb"])
    print("sections", out["sections"])
    print("scale_factors", out["scale_factors"][:12], "...")
    print("codewords", len(out["codewords"]), "spectral_bits", out["bit_pos_after_spectral"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
