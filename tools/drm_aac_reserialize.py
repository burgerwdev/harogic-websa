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
    # The GA individual-channel-stream flags between the scale factors and the
    # spectral data (the DRM syntax carries tns_present, drops pulse/gain control).
    out["pulse_present"] = br.read(1)
    out["tns_present"] = br.read(1)
    out["gain_present"] = br.read(1)
    if out["pulse_present"] or out["gain_present"]:
        raise ValueError("pulse/gain control not supported by the re-serializer")

    # Spectral data (per band, using the 960-sample long-block offsets — DRM uses
    # the 960-sample transform, not the 1024-sample GA default).
    offsets = SFB_OFFSETS["sfb_24_960"]
    out["codewords"] = []  # (codebook after VCB11 mapping, full codeword bits)
    out["band_cb"] = []    # per-band codebook after VCB11 mapping
    for band in range(out["max_sfb"]):
        cb = codebooks[band]
        if cb == 0 or cb == 13 or cb in (14, 15):
            out["band_cb"].append(cb)
            continue
        table, dim, bits, offset = CODEBOOKS[str(cb)]
        mask = (1 << bits) - 1
        width = offsets[band + 1] - offsets[band]
        band_cws = []
        band_max_abs = 0
        for _ in range(width // dim):
            cw_start = br.pos
            idx = decode_huffman_word(br, table)
            cw_bits = br.bits(cw_start, br.pos - cw_start)
            coefs = []
            for _ in range(dim):
                coefs.append((idx & mask) - offset)
                idx >>= bits
            sign_bits = ""
            signed = []
            for coef in coefs:
                s = 0
                if offset == 0 and coef != 0:
                    s = -1 if br.read(1) else 1
                    sign_bits += "1" if s < 0 else "0"
                signed.append(coef * s if s else coef)
            esc_bits = ""
            if cb == 11:
                for j, coef in enumerate(coefs):
                    if coef == 16:
                        esc_start = br.pos
                        v = _read_escape(br, signed[j] < 0)
                        esc_bits += br.bits(esc_start, br.pos - esc_start)
                        signed[j] = v
            for v in signed:
                band_max_abs = max(band_max_abs, abs(v))
            band_cws.append(cw_bits + sign_bits + esc_bits)
        mapped_cb = vcb11_for(band_max_abs) if cb == 11 else cb
        out["band_cb"].append(mapped_cb)
        for cw in band_cws:
            out["codewords"].append((mapped_cb, cw))
    out["bit_pos_after_spectral"] = br.pos
    return out


def _read_escape(br: BitReader, sign: int) -> int:
    """Read one escape sequence (fdK-AAC CBlock_GetEscape) and return the full
    signed coefficient value: (i-4) leading ones, a zero, then i value bits."""
    i = 4
    while i < 13 and br.read(1) == 1:
        i += 1
    if i == 13:
        return 8192
    val = br.read(i) + (1 << i)
    return -val if sign else val


def cb_lav(cb: int) -> int:
    """Largest absolute value a spectral codebook can carry (ISO 14496-3 Table
    4.A.1 + the VCB11 table; fdK-AAC aLargestAbsoluteValue)."""
    return [0, 1, 1, 2, 2, 4, 4, 7, 7, 12, 12, 8191, 0, 0, 0, 0,
            15, 31, 47, 63, 95, 127, 159, 191, 223, 255, 319, 383, 511,
            767, 1023, 2047][cb & 31]


def vcb11_for(max_abs: int) -> int:
    """Smallest virtual codebook (16..31) able to carry max_abs, or 11 above 2047."""
    for cb in range(16, 32):
        if cb_lav(cb) >= max_abs:
            return cb
    return 11


def build_sections_vcb11(band_cb: list) -> list:
    """Run-length the (VCB11-mapped) per-band codebooks; codebook 11 and the
    virtual codebooks 16..31 are one band each (implicit length)."""
    sections = []
    for cb in band_cb:
        if sections and sections[-1][0] == cb and not (cb == 11 or cb >= 16):
            sections[-1][1] += 1
        else:
            sections.append([cb, 1])
    return sections


def encode_sections_vcb11(sections: list) -> str:
    """Re-encode section data as VCB11: 5-bit codebooks, 5-bit lengths (escape
    31); codebook 11 and virtual 16..31 carry no length (implicit one band)."""
    bits = ""
    for cb, length in sections:
        bits += f"{cb:05b}"
        if cb == 11 or cb >= 16:
            continue
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
    """Assemble the complete DRM AAC frame (ES 201 980 §5.3.1; DecDRM's
    `el_drm_sce` layout): [id+tag][ics_info tns_present ltp_present global_gain
    section_data scale_factor_data hcr_lengths] spectral_data(HCR), with the
    bracketed part covered by aac_crc_bits. The DRM order moves global_gain after
    the tns/ltp flags and omits the GA predictor bit."""
    br = BitReader(source)
    # (the GA id_syn_ele + element_tag are not part of the DRM access unit)
    ics_info = br.bits(15, 10)          # ics_reserved + window_sequence + shape + max_sfb
    tns_ltp = ("1" if out["tns_present"] else "0") + "0"  # tns_data_present + ltp_data_present
    global_gain = br.bits(7, 8)         # global_gain (moved after tns/ltp)
    sections = encode_sections_vcb11(build_sections_vcb11(out["band_cb"]))
    sf = br.bits(out["bit_pos_after_sections"],
                 out["bit_pos_after_scale_factors"] - out["bit_pos_after_sections"])
    reordered, lrsd, llc = hcr_encode(out["codewords"])
    hcr_side = f"{lrsd:014b}{llc:06b}"
    side_info = ics_info + tns_ltp + global_gain + sections + sf + hcr_side
    crc = crc8(side_info)
    return f"{crc:08b}" + side_info + reordered  # no id_syn_ele/tag in the DRM access unit


def hcr_encode(codewords: list) -> tuple:
    """Full HCR encoder (ISO 14496-3 error-resilient spectral data; the decoder
    control flow of fdK-AAC aacdec_hcr.cpp inverted). Returns (bit_string,
    reordered_length, longest_codeword_length)."""
    # Codebook priority: 11 first, then virtual 31..16, then 9/10, 7/8, 5/6, 3/4, 1/2.
    priority = [0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 22, 0, 0, 0, 0,
                6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]
    # Sort by priority (descending), ties in natural order.
    sorted_cws = []
    for prio in range(22, 0, -1):
        for cb, cw in codewords:
            if priority[cb & 31] == prio:
                sorted_cws.append((cb, cw))
    total = sum(len(cw) for _, cw in sorted_cws)
    if total == 0:
        return "", 0, 0
    longest = max(len(cw) for _, cw in sorted_cws)

    # Segmentation grid: one segment per priority codeword, width min(aMaxCwLen, longest).
    segs = []
    start = 0
    for cb, _cw in sorted_cws:
        width = min(AMAX_CW_LEN[cb & 31], longest)
        if start + width <= total:
            segs.append([start, start + width - 1, width])
            start += width
        else:
            last = segs[-1]
            w = total - last[0]
            last[1] = last[0] + w - 1
            last[2] = w
            break
    n = len(segs)
    num_sets = (len(sorted_cws) - 1) // n + 1

    out = ["0"] * total
    # Priority codewords: codeword i starts segment i, left to right.
    for seg, (_cb, cw) in zip(segs, sorted_cws, strict=False):
        for i in range(len(cw)):
            out[seg[0]] = cw[i]
            seg[0] += 1
        seg[2] -= len(cw)

    # Non-priority codewords in sets, alternating direction per set.
    seg_active = [s[2] != 0 for s in segs]
    dir_ltr = False  # first set reads right-to-left
    next_i = n
    for _set in range(1, num_sets):
        count = min(len(sorted_cws) - next_i, n)
        set_cws = [cw for _cb, cw in sorted_cws[next_i:next_i + count]]
        next_i += count
        cursor = [0] * count
        pending = [True] * count
        for trial in range(n):
            for s in range(n):
                k = (s + n - trial % n) % n
                if not seg_active[s] or k >= count or not pending[k]:
                    continue
                cw = set_cws[k]
                seg = segs[s]
                while seg[2] > 0 and cursor[k] < len(cw):
                    if dir_ltr:
                        pos = seg[0]
                        seg[0] += 1
                    else:
                        pos = seg[1]
                        seg[1] -= 1
                    out[pos] = cw[cursor[k]]
                    cursor[k] += 1
                    seg[2] -= 1
                if cursor[k] == len(cw):
                    pending[k] = False
                if seg[2] == 0:
                    seg_active[s] = False
        dir_ltr = not dir_ltr
    return "".join(out), total, longest


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/raw.aac"
    with open(path, "rb") as f:
        data = f.read()
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
