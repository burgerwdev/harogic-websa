#!/usr/bin/env python3
"""Generate FT8 fixtures: a real FT8 message encoded into an 8-FSK waveform.

The decoder lives in the browser (`wasm/src/digital/ft8`), so its input has to be committed: this
script encodes a standard FT8 message the way the protocol specifies and writes both the waveform
and what it must decode to.

Encoding steps, all from the FT8 specification (tables ported from ft8_lib, MIT — see
`tools/fixtures/port_ft8_tables.py`):

  1. message  -> 77-bit payload        (standard type 1: call_to 28+1, call_de 28+1, ir+grid 16, i3 3)
  2. payload  -> 91 bits               (14-bit CRC-14 appended, computed over 82 bits)
  3. 91 bits  -> 174-bit codeword      (LDPC(174,91), systematic)
  4. 174 bits -> 58 data symbols       (3 bits each through the Gray map)
  5. symbols  -> 79 tones              (Costas sync blocks at 0-6, 36-42, 72-78)
  6. tones    -> complex baseband      (8-FSK, 6.25 Hz spacing, 0.160 s per symbol)

    python3 tools/fixtures/gen_ft8_fixtures.py [--check]
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
TABLES = json.loads((ROOT / 'tests' / 'fixtures' / 'ft8' / 'tables.json').read_text())
OUT = ROOT / 'tests' / 'fixtures' / 'ft8'

#: The message the fixture carries, and where it sits in the 48 kHz baseband.
MESSAGE = 'CQ JO1WKO PM95'
BASE_HZ = 1_000.0
SAMPLE_RATE = 48_000.0
SYMBOL_SECONDS = 0.160
SYMBOL_SAMPLES = int(SAMPLE_RATE * SYMBOL_SECONDS)      # 7680
TONE_SPACING_HZ = 6.25
NUM_SYMBOLS = 79
NUM_DATA_SYMBOLS = 58
SYNC_LENGTH = 7
SYNC_OFFSET = 36
COSTAS = TABLES['costas']
GRAY = TABLES['gray']
CRC_POLYNOMIAL = TABLES['crc_polynomial']
CRC_WIDTH = TABLES['crc_width']
GENERATOR = TABLES['ldpc_generator']

NTOKENS = 2_063_592
MAX22 = 4_194_304
MAXGRID4 = 32_400

# Character tables (text.c's `nchar`): ALPHANUM_SPACE = " 0-9 A-Z", ALPHANUM = "0-9 A-Z",
# NUMERIC = "0-9", LETTERS_SPACE = " A-Z".
ALPHANUM_SPACE = ' ' + '0123456789' + 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
ALPHANUM = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ'
NUMERIC = '0123456789'
LETTERS_SPACE = ' ' + 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'


def pack_basecall(callsign: str) -> int:
    """Standard callsign -> the base-call number (the reference's `pack_basecall`)."""
    c6 = [' '] * 6
    length = len(callsign)
    if length > 6:
        raise SystemExit(f'not a standard callsign: {callsign!r}')
    if length > 2 and callsign[2].isdigit():
        c6[:length] = callsign                       # AB0XYZ
    elif length > 1 and callsign[1].isdigit():
        c6[1:1 + length] = callsign                   # A0XYZ -> " A0XYZ"
    else:
        raise SystemExit(f'not a standard callsign: {callsign!r}')
    try:
        i0 = ALPHANUM_SPACE.index(c6[0])
        i1 = ALPHANUM.index(c6[1])
        i2 = NUMERIC.index(c6[2])
        i3 = LETTERS_SPACE.index(c6[3])
        i4 = LETTERS_SPACE.index(c6[4])
        i5 = LETTERS_SPACE.index(c6[5])
    except ValueError as exc:                        # pragma: no cover - fixture inputs are known
        raise SystemExit(f'unsupported callsign {callsign!r}: {exc}') from exc
    n28 = i0
    for value, base in ((i1, 36), (i2, 10), (i3, 27), (i4, 27), (i5, 27)):
        n28 = n28 * base + value
    return NTOKENS + MAX22 + n28


def pack28(callsign: str) -> tuple[int, int]:
    """`(n28, ip)` for a callsign: tokens, CQ modifiers, or a standard base call."""
    if callsign == 'DE':
        return 0, 0
    if callsign == 'QRZ':
        return 1, 0
    if callsign == 'CQ':
        return 2, 0
    if callsign.startswith('CQ ') and len(callsign) < 8:
        argument = callsign[3:]
        if argument.isdigit() and len(argument) == 3:
            return 3 + int(argument), 0
        if len(argument) == 4 and argument.isalnum():
            value = 0
            for char in argument:
                value = value * 27 + (ALPHANUM_SPACE.index(char) if char in ALPHANUM_SPACE else 0)
            return 1003 + value, 0
        raise SystemExit(f'unsupported CQ modifier: {callsign!r}')
    ip = 0
    base = callsign
    if base.endswith(('/P', '/R')):
        ip = 1
        base = base[:-2]
    return pack_basecall(base), ip


def packgrid(extra: str) -> int:
    """`igrid4` for a grid, a report, RRR/RR73/73, or nothing (the reference's `packgrid`)."""
    if not extra:
        return MAXGRID4 + 1
    if extra == 'RRR':
        return MAXGRID4 + 2
    if extra == 'RR73':
        return MAXGRID4 + 3
    if extra == '73':
        return MAXGRID4 + 4
    if (len(extra) == 4 and 'A' <= extra[0] <= 'R' and 'A' <= extra[1] <= 'R'
            and extra[2].isdigit() and extra[3].isdigit()):
        igrid4 = ord(extra[0]) - ord('A')
        igrid4 = igrid4 * 18 + (ord(extra[1]) - ord('A'))
        igrid4 = igrid4 * 10 + int(extra[2])
        igrid4 = igrid4 * 10 + int(extra[3])
        return igrid4
    sign = 1
    text = extra
    if text.startswith('R'):
        sign = -1                      # ir = 1 is carried in bit 15 by the caller
        text = text[1:]
    if len(text) == 3 and (text[0] in '+-') and text[1:].isdigit():
        dd = int(text)
        return (MAXGRID4 + 35 + dd) | (0x8000 if sign < 0 else 0)
    raise SystemExit(f'unsupported grid/report: {extra!r}')


def standard_payload(message: str) -> tuple[int, list[int]]:
    """77-bit payload for a standard message: `call_to call_de extra`."""
    parts = message.split()
    if len(parts) == 3:
        call_to, call_de, extra = parts
    elif len(parts) == 2:
        call_to, call_de, extra = parts[0], parts[1], ''
    else:
        raise SystemExit(f'unsupported message: {message!r}')
    n28a, ipa = pack28(call_to)
    n28b, ipb = pack28(call_de)
    igrid4 = packgrid(extra)
    i3 = 1
    if call_to.endswith('/P') or call_de.endswith('/P'):
        i3 = 2
    n29a = (n28a << 1) | ipa
    n29b = (n28b << 1) | ipb
    ir = 1 if igrid4 & 0x8000 else 0
    grid15 = igrid4 & 0x7FFF
    bits = (n29a << 48) | (n29b << 19) | (ir << 18) | (grid15 << 3) | i3
    return bits, [(bits >> (76 - i)) & 1 for i in range(77)]


def crc14(bits77: list[int]) -> int:
    """CRC-14 over the 77 payload bits zero-extended to 82 (the reference's `ftx_add_crc`).

    A literal port of the reference loop (`crc.c::ftx_compute_crc`): the message is handled as
    *bytes*, a whole byte being XORed into the top of the 14-bit remainder every 8 bits, and the
    polynomial applied when bit 13 is set. Reformulating it as a bit-serial CRC produced a
    non-interoperable value twice (28 of 79 tones disagreed with the reference), so this stays
    structurally identical to the original.
    """
    message = bytearray(11)
    for index, bit in enumerate(bits77):
        if bit:
            message[index // 8] |= 1 << (7 - (index % 8))
    remainder = 0
    for idx_bit in range(82):
        if idx_bit % 8 == 0:
            remainder ^= message[idx_bit // 8] << (CRC_WIDTH - 8)
        if remainder & (1 << (CRC_WIDTH - 1)):
            remainder = ((remainder << 1) ^ CRC_POLYNOMIAL) & 0xFFFF
        else:
            remainder = (remainder << 1) & 0xFFFF
    return remainder & ((1 << CRC_WIDTH) - 1)


def ldpc_encode(bits91: list[int]) -> list[int]:
    """Systematic LDPC(174,91): the first 91 bits are the payload, the rest are parity."""
    codeword = list(bits91)
    for row in GENERATOR:
        parity = 0
        for byte_index, byte in enumerate(row):
            for bit_index in range(8):
                k = byte_index * 8 + bit_index
                if k < len(bits91) and (byte >> (7 - bit_index)) & 1:
                    parity ^= bits91[k]
        codeword.append(parity)
    return codeword


def tones_for(codeword: list[int]) -> list[int]:
    """58 data symbols (3 bits each through the Gray map) with the Costas blocks inserted."""
    tones = [0] * NUM_SYMBOLS
    for block in range(3):
        start = block * SYNC_OFFSET
        tones[start:start + SYNC_LENGTH] = COSTAS
    data_index = 0
    for position in range(NUM_SYMBOLS):
        if position < SYNC_LENGTH or SYNC_OFFSET <= position < SYNC_OFFSET + SYNC_LENGTH \
                or 2 * SYNC_OFFSET <= position < 2 * SYNC_OFFSET + SYNC_LENGTH:
            continue
        bits3 = (codeword[data_index] << 2) | (codeword[data_index + 1] << 1) | codeword[data_index + 2]
        data_index += 3
        tones[position] = GRAY[bits3]
    return tones


def encode(message: str, base_hz: float) -> tuple[list[int], list[int], list[int], np.ndarray]:
    """Message -> (payload bits, codeword, tones, complex baseband), the full tx chain."""
    payload_bits, payload_bits77 = standard_payload(message)
    a91 = payload_bits77 + [(crc14(payload_bits77) >> (13 - i)) & 1 for i in range(14)]
    codeword = ldpc_encode(a91)
    tones = tones_for(codeword)
    samples = waveform(tones, base_hz)
    return payload_bits77, codeword, tones, samples


def waveform(tones: list[int], base_hz: float) -> np.ndarray:
    """Complex baseband: one tone per symbol, phase-continuous, at `base_hz + k*6.25`."""
    total = len(tones) * SYMBOL_SAMPLES
    out = np.empty(total, dtype=np.complex128)
    phase = 0.0
    index = 0
    for tone in tones:
        frequency = base_hz + tone * TONE_SPACING_HZ
        step = 2.0 * np.pi * frequency / SAMPLE_RATE
        phases = phase + step * np.arange(SYMBOL_SAMPLES)
        out[index:index + SYMBOL_SAMPLES] = np.exp(1j * phases)
        phase = (phase + step * SYMBOL_SAMPLES) % (2.0 * np.pi)
        index += SYMBOL_SAMPLES
    return out


# Four transmissions in one 15 s slot: the busy-band scenario. Real crowded bands pack signals a
# few tens of Hz apart -- an FT8 signal only spans 50 Hz -- so three sit at 800/875/950 Hz, and the
# fourth shares the strong one's *exact* frequency (800 Hz): the co-channel pair is the case only a
# subtracting decoder can untangle, because their waterfalls superpose bin for bin. Whole-sample
# offsets; relative amplitudes in dB.
MULTI_SIGNALS = [
    {'message': 'CQ JO1WKO PM95', 'freq_hz': 800.0, 'amp_db': 0.0, 'offset_s': 0.0},
    {'message': 'JA1XYZ BG7BVP -10', 'freq_hz': 950.0, 'amp_db': -8.0, 'offset_s': 0.4},
    {'message': 'BG7BVP JA1XYZ -15', 'freq_hz': 875.0, 'amp_db': -16.0, 'offset_s': 1.2},
    {'message': 'K1ABC W9XYZ -20', 'freq_hz': 800.0, 'amp_db': -16.0, 'offset_s': 2.0},
]

#: A second message as its own waveform, so the subtraction tests can build two-transmission
#: scenarios (co-channel, or same-frame near-co-channel) from two distinct texts. Two copies of the
#: same waveform would decode to the same text and be deduplicated before subtraction could matter.
MESSAGE_2 = 'K1ABC W9XYZ -20'
SECOND_BASE_HZ = 1_000.0


def build_multi() -> tuple[dict[str, bytes], dict]:
    """Three co-slot transmissions at different frequencies and levels, summed, no noise.

    The noise floor is the sweep test's business (`wasm/tests/ft8_snr_sweep.rs` adds deterministic
    band-limited noise); the fixture itself is the deterministic sum of the three waveforms, so the
    expected texts and levels are exact and the byte-compare in `--check` is stable.
    """
    slot = int(SAMPLE_RATE * 15.0)
    mix = np.zeros(slot, dtype=np.complex128)
    expected = []
    for spec in MULTI_SIGNALS:
        _, _, _, samples = encode(spec['message'], spec['freq_hz'])
        start = int(spec['offset_s'] * SAMPLE_RATE)
        mix[start:start + samples.size] += samples * (10.0 ** (spec['amp_db'] / 20.0))
        expected.append({
            'message': spec['message'],
            'freq_hz': spec['freq_hz'],
            'amp_db': spec['amp_db'],
            'offset_s': spec['offset_s'],
        })
    interleaved = np.empty(mix.size * 2, dtype=np.float32)
    interleaved[0::2] = mix.real.astype(np.float32)
    interleaved[1::2] = mix.imag.astype(np.float32)
    manifest = {
        'note': 'Generated by tools/fixtures/gen_ft8_fixtures.py: four FT8 transmissions in one slot -- '
                '800/875/950 Hz at 0/-8/-16 dB (offset 0/0.4/1.2 s) plus a fourth sharing the '
                'strong signal\'s exact 800 Hz at -16 dB (offset 2.0 s): a crowded band with one '
                'co-channel pair. The decoder must report all four once it decodes a busy band '
                'properly; the noise floor for SNR scenarios is added by '
                'wasm/tests/ft8_snr_sweep.rs.',
        'iq_file': 'ft8_multi3_iq.bin',
        # Not "samples"/"rate": the naive manifest readers in the Rust tests find the FIRST
        # occurrence of a key, and this object sorts before the top-level ones.
        'rate_hz': SAMPLE_RATE,
        'slot_samples': int(mix.size),
        'signals': expected,
    }
    return {'ft8_multi3_iq.bin': interleaved.tobytes()}, manifest


def build() -> tuple[dict[str, bytes], dict]:
    payload_bits77, codeword, tones, samples = encode(MESSAGE, BASE_HZ)

    interleaved = np.empty(samples.size * 2, dtype=np.float32)
    interleaved[0::2] = samples.real.astype(np.float32)
    interleaved[1::2] = samples.imag.astype(np.float32)

    multi_files, multi_manifest = build_multi()
    _, _, _, samples_2 = encode(MESSAGE_2, SECOND_BASE_HZ)
    interleaved_2 = np.empty(samples_2.size * 2, dtype=np.float32)
    interleaved_2[0::2] = samples_2.real.astype(np.float32)
    interleaved_2[1::2] = samples_2.imag.astype(np.float32)
    multi_manifest['second_iq_file'] = 'ft8_second_iq.bin'
    multi_manifest['second_message'] = MESSAGE_2
    multi_manifest['second_base_hz'] = SECOND_BASE_HZ
    multi_manifest['second_samples'] = int(samples_2.size)
    files = {'ft8_cq_iq.bin': interleaved.tobytes(),
             'ft8_second_iq.bin': interleaved_2.tobytes(), **multi_files}
    manifest = {
        'note': 'Generated by tools/fixtures/gen_ft8_fixtures.py: a standard FT8 message encoded per the '
                'protocol (tables from ft8_lib, MIT). The decoder in wasm/src/digital/ft8 must '
                'recover `message` exactly.',
        'message': MESSAGE,
        'payload_bits': payload_bits77,
        'codeword_bits': codeword,
        'tones': tones,
        'rate': SAMPLE_RATE,
        'base_hz': BASE_HZ,
        'symbol_seconds': SYMBOL_SECONDS,
        'symbol_samples': SYMBOL_SAMPLES,
        'tone_spacing_hz': TONE_SPACING_HZ,
        'symbols': NUM_SYMBOLS,
        'samples': int(samples.size),
        'iq_file': 'ft8_cq_iq.bin',
        'slot_seconds': 15.0,
        'multi': multi_manifest,
    }
    return files, manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true',
                        help='verify the committed fixtures instead of writing them')
    args = parser.parse_args()
    files, manifest = build()
    manifest_bytes = json.dumps(manifest, indent=1, sort_keys=True).encode() + b'\n'
    if args.check:
        problems = [name for name, data in files.items()
                    if not (OUT / name).exists() or (OUT / name).read_bytes() != data]
        if not (OUT / 'manifest.json').exists() or (OUT / 'manifest.json').read_bytes() != manifest_bytes:
            problems.append('manifest.json')
        if problems:
            print('ft8 fixture drift:', ', '.join(problems), file=sys.stderr)
            return 1
        print(f'ft8 fixtures OK: {manifest["message"]} -> {manifest["samples"]} complex samples')
        return 0
    OUT.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (OUT / name).write_bytes(data)
        print(f'{name}: {len(data)} bytes')
    (OUT / 'manifest.json').write_bytes(manifest_bytes)
    print(f'manifest.json ({manifest["message"]}, {manifest["symbols"]} symbols)')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
