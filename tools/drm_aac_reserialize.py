#!/usr/bin/env python3
"""Parse a raw MPEG-4 AAC-LC access unit and dump its frame header and section
data — the first step of re-serialising to DRM AAC syntax (VCB11 + HCR).

Status: WORK IN PROGRESS. This parses the frame header (ICS info) and the section
data correctly, but the scale factors, spectral Huffman data and the VCB11/HCR
re-serialisation are not yet implemented. Only codebook 1 is transcribed; the
remaining 11 codebooks must be extracted from fdK-AAC's aac_rom.cpp. Reference:
ISO/IEC 14496-3 §4.5; see docs/en/DRM_AAC_RESERIALIZATION.md for the full plan.
"""
from __future__ import annotations

import re
import sys


# ---------------------------------------------------------------------------
# Huffman codebook tables, transcribed from fdK-AAC aac_rom.cpp. Each entry is a
# quad-tree node: read 2 bits to index one of 4 children; a child value with bit0
# set is a leaf (bit1 set = push back one bit, the decoded value is value >> 2),
# otherwise it is an internal node index (value >> 2).
# ---------------------------------------------------------------------------

def _t(hexes: str) -> list:
    return [[int(x, 16) for x in row.split()] for row in hexes.strip().splitlines()]


CB1 = _t("""
0157 0157 0004 0018
0008 000c 0010 0014
015b 015b 0153 0153
0057 0057 0167 0167
0257 0257 0117 0117
0197 0197 0147 0147
001c 0030 0044 0058
0020 0024 0028 002c
014b 014b 0163 0163
0217 0217 0127 0127
0187 0187 0097 0097
016b 016b 0017 0017
0034 0038 003c 0040
0143 0143 0107 0107
011b 011b 0067 0067
0193 0193 0297 0297
019b 019b 0247 0247
0048 004c 0050 0054
01a7 01a7 0267 0267
0113 0113 025b 025b
0053 0053 005b 005b
0253 0253 0047 0047
005c 0070 0084 0098
0060 0064 0068 006c
012b 012b 0123 0123
018b 018b 00a7 00a7
0227 0227 0287 0287
0087 0087 010b 010b
0074 0078 007c 0080
021b 021b 0027 0027
0157 0157 016b 016b
0173 0173 015b 015b
0257 0257 025b 025b
0193 0193 0197 0197
0113 0113 0117 0117
008b 008b 0023 0023
00a3 00a3 0013 0013
00c7 00c7 0007 0007
0088 008c 0090 0094
0183 0183 0103 0103
01a3 01a3 0063 0063
0043 0043 0053 0053
0123 0123 0093 0093
0253 0253 0263 0263
0167 0167 0107 0107
009c 00b0 00c4 00d8
00a0 00a4 00a8 00ac
0213 0213 0177 0177
0223 0223 0227 0227
019b 019b 0187 0187
0163 0163 0167 0167
""")

# Only codebook 1 is shown here for the prototype; the remaining 11 are appended
# by tools/extract_aac_huff.py from fdK-AAC's aac_rom.cpp.
CODEBOOKS = {
    1: (CB1, 4, 2, 1),
}


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


def decode_huffman_word(br: BitReader, codebook) -> int:
    """Inverse of fdK-AAC's CBlock_DecodeHuffmanWordCB."""
    index = 0
    while True:
        index = codebook[index][br.read(2)]
        if index & 1:
            break
        index >>= 2
    if index & 2:
        br.pos -= 1
    return index >> 2


def parse_aac_frame(data: bytes) -> dict:
    br = BitReader(data)
    out = {"id_syn_ele": br.read(3), "element_tag": br.read(4)}
    out["global_gain"] = br.read(8)
    # ICS info (long window, SCE)
    br.read(1)  # ics_reserved_bit
    window_sequence = br.read(2)
    window_shape = br.read(1)
    out["window_sequence"] = window_sequence
    out["window_shape"] = window_shape
    if window_sequence == 2:  # eight short
        max_sfb = br.read(4)
        br.read(7)  # scale_factor_grouping
    else:
        max_sfb = br.read(6)
        br.read(1)  # predictor_data_present
    out["max_sfb"] = max_sfb
    # Section data (ISO 14496-3 §4.5.3.3.3; the long-block case). The section length
    # is `nbits` wide (5 for long blocks) with escape value `2^nbits − 1` meaning
    # "add the escape value and read again".
    nbits = 5
    esc_val = (1 << nbits) - 1
    sections = []
    sfb = 0
    while sfb < max_sfb:
        sect_cb = br.read(4)
        sect_len = 0
        while True:
            incr = br.read(nbits)
            sect_len += incr
            if incr != esc_val:
                break
        sections.append((sect_cb, sect_len))
        sfb += sect_len
    out["sections"] = sections
    out["bit_pos_after_sections"] = br.pos
    return out


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


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/raw.aac"
    data = open(path, "rb").read()
    br = BitReader(data)
    skip_fil_elements(br)
    frame = data[br.pos >> 3 :]
    print(f"SCE frame starts at byte {br.pos >> 3}, {len(frame)} bytes")
    print(parse_aac_frame(frame))
    return 0


if __name__ == "__main__":
    sys.exit(main())
