"""
measurements/framer.py -- the WebSocket binary frame codec (single source for the wire format).

Every frame the backend sends is built here, including RTA and audio. Keeping the codec in
one hardware-free module means the format can be unit tested (and locked by the golden
fixtures in tests/fixtures/frames/) without an analyzer attached.

Layout (little endian, 4-byte ASCII magic first):

  FREQ  magic + ver(u32) + points(u32) + sweep_ms(f32) + float64[points]
  POWR  magic + ver(u32) + points(u32) + sweep_ms(f32) + float32[points]
  RTAF  magic + ver(u32) + points(u32) + wfLen(u16) + maxDensity(u16) + startHz(f64)
        + float64[points] + float32[points] + uint16[wfLen] + stopHz(f64)
  AUDF  magic + seq(u32) + rate(u32) + samples(u32) + int16[samples]
  IQDF  magic + ver(u32) + seq(u32) + samples(u32) + rate(f64) + center_hz(f64)
        + int16[2*samples]

Key points:
  * POWR is forced to float32 and FREQ stays float64: np.interp would otherwise widen the
    frontend buffer and misalign the TS parse.
  * The 20-byte RTAF head after the magic keeps startHz 8-byte aligned (24-byte header).
  * IQDF stores the sample rate as f64 (an IQS rate is 62.5 MHz / 2**n, rarely an integer),
    and puts both f64 fields before the payload so the int16 block starts 2-byte aligned.
  * The TypeScript counterpart is frontend/modern/src/core/frames.ts; the two are held
    together by tools/gen_frame_fixtures.py + the tests on both sides.
"""
from __future__ import annotations

import struct

import numpy as np

MAGIC_FREQ = b'FREQ'
MAGIC_POWR = b'POWR'
MAGIC_RTA = b'RTAF'
MAGIC_AUDIO = b'AUDF'
MAGIC_IQ = b'IQDF'

#: Wire version of the IQDF payload (the DSP-input format the browser worker parses).
IQ_VERSION = 1

HEADER = struct.Struct('<IIf')          # version, points, sweep_ms (12 bytes after the magic)
RTA_HEADER = struct.Struct('<IIHHd')    # version, points, wf_len, max_density, start_hz
AUDIO_HEADER = struct.Struct('<III')    # seq, rate, samples
IQ_HEADER = struct.Struct('<IIIdd')     # version, seq, samples, rate, center_hz (28 bytes)


def _frame(magic: bytes, version: int, points: int, sweep_ms: float, data, dtype) -> bytes:
    hdr = magic + HEADER.pack(version, points, sweep_ms)
    return hdr + np.ascontiguousarray(data, dtype=dtype).tobytes()


def encode_freq(version: int, freq_hz, sweep_ms: float) -> bytes:
    return _frame(MAGIC_FREQ, version, len(freq_hz), sweep_ms, freq_hz, np.float64)


def encode_powr(version: int, power_dbm, sweep_ms: float) -> bytes:
    return _frame(MAGIC_POWR, version, len(power_dbm), sweep_ms, power_dbm, np.float32)


def encode_rta(version, freq_hz, spec_dbm, wf_row, max_density, start_hz, stop_hz) -> bytes:
    """RTA frame: real-time trace + one waterfall row + the display window."""
    points = len(spec_dbm)
    max_density = max(0, min(65535, int(max_density)))
    hdr = MAGIC_RTA + RTA_HEADER.pack(version, points, len(wf_row), max_density, float(start_hz))
    payload = (np.ascontiguousarray(freq_hz, dtype=np.float64).tobytes() +
               np.ascontiguousarray(spec_dbm, dtype=np.float32).tobytes() +
               np.ascontiguousarray(wf_row, dtype=np.uint16).tobytes())
    return hdr + payload + struct.pack('<d', float(stop_hz))


# Backwards-compatible private alias (the RTA session used to own this encoder).
_encode_rta = encode_rta


def encode_audio(seq: int, rate: int, pcm) -> bytes:
    """Mono PCM16 audio frame; seq == 0 tells the client to flush stale audio."""
    pcm = np.ascontiguousarray(pcm, dtype=np.int16)
    return MAGIC_AUDIO + AUDIO_HEADER.pack(int(seq), int(rate), pcm.size) + pcm.tobytes()


def encode_iq(version: int, seq: int, rate: float, center_hz: float, iq) -> bytes:
    """Raw interleaved int16 IQ at the DDC input rate (the browser DSP's input).

    ``iq`` is the interleaved I,Q stream, so the payload holds 2 samples per complex sample.
    ``seq == 0`` flushes stale IQ on the client, exactly like AUDF: after a retune or an IQS
    reconfiguration the queued blocks describe a different centre and must not be mixed into
    the new stream.
    """
    iq = np.ascontiguousarray(iq, dtype=np.int16).reshape(-1)
    if iq.size & 1:
        iq = iq[:-1]                        # a torn block would desync I/Q for the client
    return (MAGIC_IQ
            + IQ_HEADER.pack(int(version), int(seq), iq.size // 2,
                             float(rate), float(center_hz))
            + iq.tobytes())


def parse_header(buf: bytes):
    if len(buf) < 16:
        return None
    magic = buf[:4]
    ver, points = struct.unpack_from('<II', buf, 4)
    sweep_ms, = struct.unpack_from('<f', buf, 12)
    return magic, ver, points, sweep_ms


def decode_powr(buf: bytes, points: int) -> np.ndarray:
    return np.frombuffer(buf, dtype=np.float32, offset=16, count=points)
