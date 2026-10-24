#!/usr/bin/env python3
"""SSTV encoding for the Python bench tools: mode tables -> tones -> 16-bit PCM.

This is the transmit side of the repo's own reference pair. The decoder is the vendored Rust tree
(`dsp/core/src/digital/sstv/vendor`); the tables here are *ported from* that tree by
`tools/fixtures/port_sstv_modes.py`, so a mode's timing exists once and the transmitter cannot drift
from the decoder. `gen_sstv_fixtures.py` writes a committed waveform with this module and the Rust
decoder's test decodes it — a cross-implementation check, in the same spirit as the DSP fixtures
comparing Rust kernels against the Python reference.

What a transmission is (the Dayton paper's structure):

    VOX tones -> leader / break / leader -> VIS code -> [starting sync pulse for Scottie]
              -> one pass of the mode's timing sequence per line (group)

and every scan step spreads the line's pixels evenly over its duration as 1500 Hz (black) to
2300 Hz (white) tones. Control steps are fixed tones: 1200 Hz sync pulses, porches and separator
pulses.

The transmitter is `tools/pluto/pluto_sstv_tx.py`; this module only builds tones and samples.
"""
from __future__ import annotations

import json
import math
import struct
import zlib
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
MODES_JSON = ROOT / 'tests' / 'fixtures' / 'sstv' / 'modes.json'

#: The luminance range every SSTV mode shares.
BLACK_HZ = 1500
WHITE_HZ = 2300
#: The sync pulse every mode shares.
SYNC_HZ = 1200
#: The VIS framing bit (start and stop).
FRAMING_HZ = 1200
#: One VIS bit: start, seven data bits (least significant first), parity, stop.
VIS_BIT_NS = 30_000_000
#: The tuning tones sent before the calibration header (not part of the paper's specification).
VOX_TONES = [
    (1900, 100_000_000), (1500, 100_000_000), (1900, 100_000_000), (1500, 100_000_000),
    (2300, 100_000_000), (1500, 100_000_000), (2300, 100_000_000), (1500, 100_000_000),
]
LEADER = (1900, 300_000_000)
BREAK = (SYNC_HZ, 10_000_000)

#: Full scale of the 16-bit samples.
FULL_SCALE = 32_767


def load_modes() -> list[dict]:
    """The mode table, in the plugin's order (the ABI's mode indices)."""
    table = json.loads(MODES_JSON.read_text(encoding='utf-8'))
    return table['modes']


def find_mode(name: str | int) -> dict:
    """Look a mode up by display name (`Martin M1`), table key (`martin_1`), VIS code or index."""
    modes = load_modes()
    if isinstance(name, int):
        if 0 <= name < len(modes):
            return modes[name]
        raise SystemExit(f'no SSTV mode at index {name}')
    text = str(name).strip()
    for mode in modes:
        if text.lower() in (mode['display'].lower(), mode['key'].lower(), mode['rust_name'].lower()):
            return mode
    if text.isdigit():
        for mode in modes:
            if mode['vis'] == int(text):
                return mode
    known = ', '.join(mode['display'] for mode in modes)
    raise SystemExit(f'unknown SSTV mode {name!r}; known modes: {known}')


def mode_names() -> list[str]:
    return [mode['display'] for mode in load_modes()]


def vis_tones(vis: int) -> list[tuple[int, int]]:
    """The ten calibration-header tones carrying the seven-bit VIS code."""
    ones = bin(vis & 0x7F).count('1')
    tones = [(FRAMING_HZ, VIS_BIT_NS)]
    for bit in range(7):
        tones.append((1100 if (vis >> bit) & 1 else 1300, VIS_BIT_NS))
    tones.append((1100 if ones % 2 == 1 else 1300, VIS_BIT_NS))
    tones.append((FRAMING_HZ, VIS_BIT_NS))
    return tones


def header_tones(mode: dict) -> list[tuple[int, int]]:
    """Everything sent before the image: VOX, leader/break/leader, VIS, starting sync pulse."""
    tones = list(VOX_TONES) + [LEADER, BREAK, LEADER] + vis_tones(mode['vis'])
    if mode['starting_sync_pulse']:
        sync = next(step['control'] for step in mode['sequence']
                    if 'control' in step and step['control']['hz'] == SYNC_HZ)
        tones.append((SYNC_HZ, sync['ns']))
    return tones


def value_frequency(value: int) -> int:
    """The tone a pixel value is sent as: 1500 Hz black .. 2300 Hz white, linearly."""
    return BLACK_HZ + (WHITE_HZ - BLACK_HZ) * value // 255


def _luma(r: int, g: int, b: int) -> int:
    return min((30 * r + 59 * g + 11 * b) // 100, 255)


def _chroma_red(r: int, luma: int) -> int:
    return max(0, min(255, (10 * (r - luma) + 7 * 255) // 14))


def _chroma_blue(b: int, luma: int) -> int:
    return max(0, min(255, (100 * (b - luma) + 89 * 255) // 178))


def _clamp_floor_div(numerator: int, denominator: int) -> int:
    """Rust's integer division truncates toward zero; Python's floors. The results here are clamped
    to 0..255 either way, so only the intermediate sign differs — this keeps the intent explicit."""
    quotient = abs(numerator) // denominator
    return quotient if numerator >= 0 else -quotient


def scan_values(mode: dict, rows: list[bytes], width: int) -> dict[str, np.ndarray]:
    """The per-column value of every scan channel, from the rows of one line group."""
    channels = [step['scan']['channel'] for step in mode['sequence'] if 'scan' in step]
    pixels = [np.frombuffer(rows[row], dtype=np.uint8).reshape(width, 3).astype(np.int32) for row in range(len(rows))]
    luma = [np.clip((30 * p[:, 0] + 59 * p[:, 1] + 11 * p[:, 2]) // 100, 0, 255) for p in pixels]
    red = [np.clip((10 * (pixels[i][:, 0] - luma[i]) + 7 * 255) // 14, 0, 255) for i in range(len(rows))]
    blue = [np.clip((100 * (pixels[i][:, 2] - luma[i]) + 89 * 255) // 178, 0, 255) for i in range(len(rows))]

    def average(planes: list[np.ndarray]) -> np.ndarray:
        stacked = np.stack(planes)
        return (stacked.sum(axis=0) // len(planes)).astype(np.int32)

    values: dict[str, np.ndarray] = {}
    for channel in channels:
        if channel == 'Red':
            values[channel] = pixels[0][:, 0]
        elif channel == 'Green':
            values[channel] = pixels[0][:, 1]
        elif channel == 'Blue':
            values[channel] = pixels[0][:, 2]
        elif channel == 'Y':
            values[channel] = luma[0]
        elif channel == 'YSecond':
            values[channel] = luma[1] if len(rows) > 1 else luma[0]
        elif channel == 'RY':
            values[channel] = average(red) if mode['color'] == 'YuvSharedPair' else red[0]
        elif channel == 'BY':
            values[channel] = average(blue) if mode['color'] == 'YuvSharedPair' else blue[0]
        else:
            raise SystemExit(f'{mode["display"]}: unknown scan channel {channel}')
    return values


def image_tones(mode: dict, rgb: bytes, width: int, height: int) -> list[tuple[int, int]]:
    """The image part of a transmission: one pass of the timing sequence per line group."""
    if width != mode['width'] or height != mode['height']:
        raise SystemExit(f'{mode["display"]} wants {mode["width"]}x{mode["height"]}, got {width}x{height}')
    rows = [rgb[row * width * 3:(row + 1) * width * 3] for row in range(height)]
    tones: list[tuple[int, int]] = []
    for start in range(0, height, mode['lines_per_sequence']):
        group = rows[start:start + mode['lines_per_sequence']]
        values = scan_values(mode, group, width)
        for step in mode['sequence']:
            if 'control' in step:
                tones.append((step['control']['hz'], step['control']['ns']))
                continue
            channel = step['scan']['channel']
            per_pixel = step['scan']['ns'] // width
            for column in range(width):
                tones.append((value_frequency(int(values[channel][column])), per_pixel))
    return tones


def transmission_tones(mode: dict, rgb: bytes, width: int, height: int) -> list[tuple[int, int]]:
    return header_tones(mode) + image_tones(mode, rgb, width, height)


def synth(tones: list[tuple[int, int]], rate: int) -> np.ndarray:
    """Continuous-phase sine synthesis at `rate`.

    The number of samples per tone uses the same integer carry arithmetic the Rust encoder uses, so a
    tone's length is exact rather than accumulating a rounding error per tone.
    """
    total = sum(ns for _, ns in tones)
    out = np.empty((total * rate) // 1_000_000_000 + len(tones) + 1, dtype=np.int16)
    cursor = 0
    phase = 0.0
    carry = 0
    for hz, ns in tones:
        numerator = ns * rate + carry
        count = numerator // 1_000_000_000
        carry = numerator % 1_000_000_000
        if count > 0:
            increment = 2.0 * math.pi * hz / rate
            positions = phase + increment * np.arange(count, dtype=np.float64)
            out[cursor:cursor + count] = np.round(np.sin(positions) * FULL_SCALE).astype(np.int16)
            phase = float(positions[-1] + increment)
            cursor += count
    return out[:cursor]


# ── pictures ─────────────────────────────────────────────────────────────────────────────────────

#: A 5x7 bitmap font, enough for the callsigns and labels a test pattern carries. Each glyph is seven
#: rows of five bits, most significant bit on the left.
FONT = {
    ' ': ['00000'] * 7,
    '-': ['00000', '00000', '00000', '11111', '00000', '00000', '00000'],
    '/': ['00001', '00010', '00100', '01000', '10000', '00000', '00000'],
    '.': ['00000', '00000', '00000', '00000', '00000', '01100', '01100'],
    '0': ['01110', '10001', '10011', '10101', '11001', '10001', '01110'],
    '1': ['00100', '01100', '00100', '00100', '00100', '00100', '01110'],
    '2': ['01110', '10001', '00001', '00010', '00100', '01000', '11111'],
    '3': ['11111', '00010', '00100', '00010', '00001', '10001', '01110'],
    '4': ['00010', '00110', '01010', '10010', '11111', '00010', '00010'],
    '5': ['11111', '10000', '11110', '00001', '00001', '10001', '01110'],
    '6': ['00110', '01000', '10000', '11110', '10001', '10001', '01110'],
    '7': ['11111', '00001', '00010', '00100', '01000', '01000', '01000'],
    '8': ['01110', '10001', '10001', '01110', '10001', '10001', '01110'],
    '9': ['01110', '10001', '10001', '01111', '00001', '00010', '01100'],
    'A': ['01110', '10001', '10001', '11111', '10001', '10001', '10001'],
    'B': ['11110', '10001', '10001', '11110', '10001', '10001', '11110'],
    'C': ['01110', '10001', '10000', '10000', '10000', '10001', '01110'],
    'D': ['11100', '10010', '10001', '10001', '10001', '10010', '11100'],
    'E': ['11111', '10000', '10000', '11110', '10000', '10000', '11111'],
    'F': ['11111', '10000', '10000', '11110', '10000', '10000', '10000'],
    'G': ['01110', '10001', '10000', '10111', '10001', '10001', '01111'],
    'H': ['10001', '10001', '10001', '11111', '10001', '10001', '10001'],
    'I': ['01110', '00100', '00100', '00100', '00100', '00100', '01110'],
    'J': ['00111', '00010', '00010', '00010', '00010', '10010', '01100'],
    'K': ['10001', '10010', '10100', '11000', '10100', '10010', '10001'],
    'L': ['10000', '10000', '10000', '10000', '10000', '10000', '11111'],
    'M': ['10001', '11011', '10101', '10101', '10001', '10001', '10001'],
    'N': ['10001', '11001', '10101', '10011', '10001', '10001', '10001'],
    'O': ['01110', '10001', '10001', '10001', '10001', '10001', '01110'],
    'P': ['11110', '10001', '10001', '11110', '10000', '10000', '10000'],
    'Q': ['01110', '10001', '10001', '10001', '10101', '10010', '01101'],
    'R': ['11110', '10001', '10001', '11110', '10100', '10010', '10001'],
    'S': ['01111', '10000', '10000', '01110', '00001', '00001', '11110'],
    'T': ['11111', '00100', '00100', '00100', '00100', '00100', '00100'],
    'U': ['10001', '10001', '10001', '10001', '10001', '10001', '01110'],
    'V': ['10001', '10001', '10001', '10001', '10001', '01010', '00100'],
    'W': ['10001', '10001', '10001', '10101', '10101', '11011', '10001'],
    'X': ['10001', '10001', '01010', '00100', '01010', '10001', '10001'],
    'Y': ['10001', '10001', '01010', '00100', '00100', '00100', '00100'],
    'Z': ['11111', '00001', '00010', '00100', '01000', '10000', '11111'],
}


class Picture:
    """An RGB888 picture in row-major order."""

    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height
        self.rgb = bytearray(width * height * 3)

    def set(self, x: int, y: int, r: int, g: int, b: int) -> None:
        if 0 <= x < self.width and 0 <= y < self.height:
            at = (y * self.width + x) * 3
            self.rgb[at:at + 3] = bytes((r & 0xFF, g & 0xFF, b & 0xFF))

    def rect(self, x: int, y: int, w: int, h: int, r: int, g: int, b: int) -> None:
        for row in range(y, y + h):
            for column in range(x, x + w):
                self.set(column, row, r, g, b)

    def text(self, x: int, y: int, text: str, scale: int, r: int, g: int, b: int) -> None:
        cursor = x
        for character in text.upper():
            glyph = FONT.get(character, FONT[' '])
            for row, bits in enumerate(glyph):
                for column, bit in enumerate(bits):
                    if bit == '1':
                        self.rect(cursor + column * scale, y + row * scale, scale, scale, r, g, b)
            cursor += 6 * scale

    def text_width(self, text: str, scale: int) -> int:
        """The width `text` occupies at `scale` (glyphs are five columns wide with a one-column gap)."""
        return max(0, len(text) * 6 * scale - scale)

    def text_fitted(self, x: int, y: int, text: str, scale: int, r: int, g: int, b: int,
                    max_width: int) -> None:
        """Draw `text`, dropping trailing characters rather than running off the picture."""
        for count in range(len(text), 0, -1):
            piece = text[:count]
            if self.text_width(piece, scale) <= max_width:
                self.text(x, y, piece, scale, r, g, b)
                return

    def bytes(self) -> bytes:
        return bytes(self.rgb)


def pattern_image(width: int, height: int, callsign: str = '', label: str = '') -> Picture:
    """A test picture: colour bars, a grey ramp, a border and (optionally) a callsign.

    A receiver judges the picture by what it can see, so the pattern is deliberately unambiguous:
    wrong geometry, swapped channels and inverted luminance are all obvious at a glance.
    """
    picture = Picture(width, height)
    bars = [(255, 255, 255), (255, 255, 0), (0, 255, 255), (0, 255, 0),
            (255, 0, 255), (255, 0, 0), (0, 0, 255), (0, 0, 0)]
    top = height * 3 // 5
    for index, (r, g, b) in enumerate(bars):
        x0 = width * index // len(bars)
        x1 = width * (index + 1) // len(bars)
        picture.rect(x0, 0, x1 - x0, top, r, g, b)
    # Grey ramp across the bottom, with a white and a black square at the ends.
    for x in range(width):
        value = x * 255 // max(1, width - 1)
        picture.rect(x, top, 1, height - top, value, value, value)
    square = max(4, height // 12)
    picture.rect(0, 0, square, square, 255, 255, 255)
    picture.rect(width - square, height - square, square, square, 0, 0, 0)
    # A one-pixel border: a picture that is shifted or stretched loses an edge.
    picture.rect(0, 0, width, 1, 255, 255, 255)
    picture.rect(0, height - 1, width, 1, 255, 255, 255)
    picture.rect(0, 0, 1, height, 255, 255, 255)
    picture.rect(width - 1, 0, 1, height, 255, 255, 255)
    scale = max(1, height // 64)
    room = width - 4 * scale
    if label:
        # Centred, and trimmed to the picture's width: a label that runs off the edge reads as a
        # transmission fault when it is only a long caption.
        x = max(scale, (width - picture.text_width(label.upper(), scale)) // 2)
        picture.text_fitted(x, top // 2 - scale * 4, label, scale, 0, 0, 0, room)
    if callsign:
        picture.text_fitted(scale * 2, height - 9 * scale, callsign, scale, 0, 0, 0, room)
    return picture


def read_image(path: Path) -> Picture:
    """Read a picture from a PNG (8-bit, non-interlaced) or binary PPM (P6) file, no dependencies."""
    data = path.read_bytes()
    if data.startswith(b'\x89PNG\r\n\x1a\n'):
        width, height, pixels = _read_png(data)
    elif data.startswith(b'P6'):
        width, height, pixels = _read_ppm(data)
    else:
        raise SystemExit(f'{path}: not a PNG or binary PPM (P6) file')
    picture = Picture(width, height)
    picture.rgb = bytearray(pixels)
    return picture


def _read_ppm(data: bytes) -> tuple[int, int, bytes]:
    fields: list[bytes] = []
    cursor = 2
    while len(fields) < 3:
        while cursor < len(data) and data[cursor:cursor + 1].isspace():
            cursor += 1
        if data[cursor:cursor + 1] == b'#':
            while cursor < len(data) and data[cursor] != 0x0A:
                cursor += 1
            continue
        start = cursor
        while cursor < len(data) and not data[cursor:cursor + 1].isspace():
            cursor += 1
        fields.append(data[start:cursor])
    cursor += 1
    width, height, maxval = (int(field) for field in fields)
    if maxval != 255:
        raise SystemExit(f'PPM maxval {maxval} is not supported (only 255)')
    return width, height, data[cursor:cursor + width * height * 3]


def _read_png(data: bytes) -> tuple[int, int, bytes]:
    cursor = 8
    width = height = 0
    colour_type = 0
    bit_depth = 0
    chunks = bytearray()
    while cursor + 8 <= len(data):
        length = struct.unpack('>I', data[cursor:cursor + 4])[0]
        kind = data[cursor + 4:cursor + 8]
        payload = data[cursor + 8:cursor + 8 + length]
        cursor += 12 + length
        if kind == b'IHDR':
            width, height, bit_depth, colour_type = struct.unpack('>IIBB', payload[:10])
            if bit_depth != 8 or colour_type not in (0, 2, 4, 6):
                raise SystemExit(f'PNG bit depth {bit_depth} colour type {colour_type} is not supported')
        elif kind == b'IDAT':
            chunks += payload
        elif kind == b'IEND':
            break
    raw = zlib.decompress(bytes(chunks))
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[colour_type]
    stride = width * channels
    out = bytearray(width * height * 3)
    previous = bytearray(stride)
    cursor = 0
    for row in range(height):
        filter_type = raw[cursor]
        line = bytearray(raw[cursor + 1:cursor + 1 + stride])
        cursor += 1 + stride
        for index in range(stride):
            left = line[index - channels] if index >= channels else 0
            up = previous[index]
            up_left = previous[index - channels] if index >= channels else 0
            if filter_type == 1:
                line[index] = (line[index] + left) & 0xFF
            elif filter_type == 2:
                line[index] = (line[index] + up) & 0xFF
            elif filter_type == 3:
                line[index] = (line[index] + (left + up) // 2) & 0xFF
            elif filter_type == 4:
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                predictor = left if (pa <= pb and pa <= pc) else (up if pb <= pc else up_left)
                line[index] = (line[index] + predictor) & 0xFF
        for column in range(width):
            at = column * channels
            if colour_type == 0 or colour_type == 4:
                grey = line[at]
                rgb = (grey, grey, grey)
            elif colour_type == 2:
                rgb = (line[at], line[at + 1], line[at + 2])
            else:
                rgb = (line[at], line[at + 1], line[at + 2])
            out[(row * width + column) * 3:(row * width + column) * 3 + 3] = bytes(rgb)
        previous = line
    return width, height, bytes(out)


def resize(picture: Picture, width: int, height: int) -> Picture:
    """Nearest-neighbour resize: a test picture does not need anything better, and it keeps the
    palette edges that make a mis-sized picture obvious."""
    if picture.width == width and picture.height == height:
        return picture
    out = Picture(width, height)
    for y in range(height):
        source_y = min(picture.height - 1, y * picture.height // height)
        for x in range(width):
            source_x = min(picture.width - 1, x * picture.width // width)
            at = (source_y * picture.width + source_x) * 3
            out.rgb[(y * width + x) * 3:(y * width + x) * 3 + 3] = picture.rgb[at:at + 3]
    return out


def fit(picture: Picture, mode: dict) -> Picture:
    """Resize a picture to the mode's resolution (the transmitter's own geometry wins)."""
    return resize(picture, mode['width'], mode['height'])
