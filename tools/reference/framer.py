"""
Reference WebSocket binary frame codec for tools and golden fixtures.

This hardware-free codec generates the fixtures in tests/fixtures/frames/ used
to verify the native server and browser wire format.

Layout (little endian, 4-byte ASCII magic first):

  FREQ  magic + ver(u32) + points(u32) + sweep_ms(f32) + float64[points]
  POWR  magic + ver(u32) + points(u32) + sweep_ms(f32) + float32[points]
  RTAF  magic + ver(u32) + points(u32) + wfLen(u16) + maxDensity(u16) + startHz(f64)
        + float64[points] + float32[points] + uint16[wfLen] + stopHz(f64)
  RTA2  RTAF layout with magic RTA2, then TRIG + metadata_length(u32) + UTF-8 JSON
  AUDF  magic + seq(u32) + rate(u32) + samples(u32) + int16[samples]
  IQBF/IQAX magic + ver(u32) + seq(u32) + samples(u32) + rate(f64) + center_hz(f64)
        + channel_id(u32) + epoch(u32) + input_index(u64) + delay_samples(f64)
        + float32[2*samples]

Key points:
  * POWR is forced to float32 and FREQ stays float64: np.interp would otherwise widen the
    frontend buffer and misalign the TS parse.
  * The 20-byte RTAF head after the magic keeps startHz 8-byte aligned (24-byte header).
  * IQBF carries the *channelized* baseband (the DSP_DDC output after the fine-tuning NCO), so
    the browser's demodulator sees exactly what the Python demodulator would. The coarse
    decimation and the tuning stay on this side, which is what keeps the browser's cost and the
    wire traffic independent of the device's raw IQ rate.
  * IQBF stores the baseband rate as f64 (it is an IQS rate divided by a power of two, rarely an
    integer), and puts both f64 fields before the payload so the f32 block starts 4-byte aligned.
  * The TypeScript counterpart is frontend/src/core/frames.ts; the two are held
    together by tools/fixtures/gen_frame_fixtures.py + the tests on both sides.
"""
from __future__ import annotations

import json
import struct

import numpy as np

MAGIC_FREQ = b'FREQ'
MAGIC_POWR = b'POWR'
MAGIC_RTA = b'RTAF'
MAGIC_RTA2 = b'RTA2'
MAGIC_AUDIO = b'AUDF'
MAGIC_BASEBAND = b'IQBF'
#: Aux-DDC baseband: identical layout to IQBF, different magic so the publisher can route
#: it only to sockets that subscribed (?iqaux=1) and the browser demodulator never sees it.
MAGIC_AUX = b'IQAX'

#: Wire version of the IQBF payload (the demodulator-input format the browser worker parses).
BASEBAND_VERSION = 2

HEADER = struct.Struct('<IIf')          # version, points, sweep_ms (12 bytes after the magic)
RTA_HEADER = struct.Struct('<IIHHd')    # version, points, wf_len, max_density, start_hz
AUDIO_HEADER = struct.Struct('<III')    # seq, rate, samples
BASEBAND_HEADER = struct.Struct('<IIIddIIQd')  # 52 bytes after the magic


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


def encode_rta2(version, freq_hz, spec_dbm, wf_row, max_density, start_hz, stop_hz,
                *, metadata) -> bytes:
    """RTA2 keeps the RTAF display layout and appends SDK packet metadata."""
    legacy = encode_rta(version, freq_hz, spec_dbm, wf_row, max_density, start_hz, stop_hz)
    trigger = json.dumps(metadata, ensure_ascii=False, allow_nan=False,
                         separators=(',', ':')).encode('utf-8')
    return MAGIC_RTA2 + legacy[4:] + b'TRIG' + struct.pack('<I', len(trigger)) + trigger


# Backwards-compatible private alias (the RTA session used to own this encoder).
_encode_rta = encode_rta


def encode_audio(seq: int, rate: int, pcm) -> bytes:
    """Mono PCM16 audio frame; seq == 0 tells the client to flush stale audio."""
    pcm = np.ascontiguousarray(pcm, dtype=np.int16)
    return MAGIC_AUDIO + AUDIO_HEADER.pack(int(seq), int(rate), pcm.size) + pcm.tobytes()


def encode_baseband(seq: int, rate: float, center_hz: float, iq, *,
                    channel_id: int, epoch: int, input_index: int, delay_samples: float) -> bytes:
    return _encode_baseband(MAGIC_BASEBAND, seq, rate, center_hz, iq,
                            channel_id, epoch, input_index, delay_samples)


def encode_aux(seq: int, rate: float, center_hz: float, iq, *,
               channel_id: int, epoch: int, input_index: int, delay_samples: float) -> bytes:
    """Aux-DDC channel baseband (IQAX): same wire layout as IQBF, opt-in routing."""
    return _encode_baseband(MAGIC_AUX, seq, rate, center_hz, iq,
                            channel_id, epoch, input_index, delay_samples)


def _encode_baseband(magic: bytes, seq: int, rate: float, center_hz: float, iq,
                     channel_id: int, epoch: int, input_index: int, delay_samples: float) -> bytes:
    """Encode normalized interleaved IQ with explicit stream identity and timing.

    Sequence zero is an empty boundary. Delay is measured in output samples;
    input_index is the causal raw-input index of the first output sample, or
    u64::MAX when an opaque vendor DDC cannot expose that index.
    """
    iq = np.ascontiguousarray(iq, dtype='<f4').reshape(-1)
    if iq.size & 1:
        raise ValueError('baseband requires complete I/Q pairs')
    if seq == 0 and iq.size:
        raise ValueError('baseband sequence zero must be an empty boundary')
    return (magic
            + BASEBAND_HEADER.pack(BASEBAND_VERSION, int(seq), iq.size // 2,
                                   float(rate), float(center_hz), int(channel_id),
                                   int(epoch), int(input_index), float(delay_samples))
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
