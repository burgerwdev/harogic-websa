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
  VSAD  magic + ver(u32) + kind(u32) + rows(u32) + cols(u32) + idealRows(u32) + idealCols(u32)
        + float32[5] scalars + float32[rows*cols] + float32[idealRows*idealCols]
        + uint32[1] metaLen + float32[metaLen]
  AUDF  magic + seq(u32) + rate(u32) + samples(u32) + int16[samples]

Key points:
  * POWR is forced to float32 and FREQ stays float64: np.interp would otherwise widen the
    frontend buffer and misalign the TS parse.
  * The 20-byte RTAF head after the magic keeps startHz 8-byte aligned (24-byte header).
  * VSAD (VSA data) is the Tier 1 payload frame: a measurement as a float32 matrix plus the
    scalar measurement block. ``rows``/``cols`` do not describe the capture, they describe
    what a display can use (an evenly decimated slice), so the frame stays bounded however
    deep the capture was; the measurement block carries the numbers for the whole block.
    A slot the current tier cannot fill is NaN, never a sentinel number.
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
MAGIC_VSA = b'VSAD'

HEADER = struct.Struct('<IIf')          # version, points, sweep_ms (12 bytes after the magic)
RTA_HEADER = struct.Struct('<IIHHd')    # version, points, wf_len, max_density, start_hz
AUDIO_HEADER = struct.Struct('<III')    # seq, rate, samples
VSA_HEADER = struct.Struct('<IIIIII')   # version, kind, rows, cols, ideal_rows, ideal_cols
VSA_SCALARS = struct.Struct('<fffff')   # symbol_rate_hz, cfo_hz, timing_samples, evm%, snr_db
VSA_SCALAR_COUNT = 5

#: VSAD payload kinds, in the order the ``kind`` field numbers them.
VSA_KINDS = ('constellation', 'power', 'ccdf', 'spectrogram')
#: The VSAD measurement block is positional: index i of the float32 block is this key.
#: EVM/MER/SNR are NaN until the demodulation chain (Phase 2) can fill them.
VSA_MEASURE_KEYS = ('mean_dbm', 'peak_dbm', 'peak_bin_dbm', 'peak_hz', 'floor_dbm',
                    'floor_1hz_dbm', 'centroid_hz', 'rms_v', 'duty', 'samples', 'symbols_n',
                    'points', 'evm_percent', 'mer_db', 'snr_db')
#: One measurement block slot that has no value in this tier.
VSA_NAN = float('nan')


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


def encode_vsa(version, kind, data, *, ideal=None, scalars=(), measurements=None) -> bytes:
    """VSA data frame: one Tier 1 measurement as a float32 matrix plus its scalars.

    ``data`` is ``(rows, cols)`` float32: a symbol cloud is ``(symbols, 2)`` interleaved
    I/Q in volts, a power-versus-time trace and a CCDF are ``(points, 2)`` (x, y), and a
    spectrogram is ``(rows, bins)`` in relative dB. ``ideal`` is the optional nominal grid
    on the cloud's own scale. ``scalars`` is the five constellation numbers and
    ``measurements`` fills :data:`VSA_MEASURE_KEYS` by name (anything missing stays NaN).
    """
    kind_id = VSA_KINDS.index(kind) if isinstance(kind, str) else int(kind)
    d = np.ascontiguousarray(data, dtype=np.float32)
    if d.ndim != 2:
        raise ValueError(f'VSAD data must be 2-D (rows, cols), got shape {d.shape}')
    grid = np.zeros((0, 2), dtype=np.float32) if ideal is None else np.ascontiguousarray(
        ideal, dtype=np.float32)
    if grid.ndim != 2:
        raise ValueError(f'VSAD ideal must be 2-D, got shape {grid.shape}')
    values = [VSA_NAN] * len(VSA_MEASURE_KEYS)
    for key, value in (measurements or {}).items():
        if key in VSA_MEASURE_KEYS:
            values[VSA_MEASURE_KEYS.index(key)] = float(value)
    meta = np.asarray(values, dtype=np.float32)
    fields = (list(scalars) + [VSA_NAN] * VSA_SCALAR_COUNT)[:VSA_SCALAR_COUNT]
    hdr = (MAGIC_VSA + VSA_HEADER.pack(int(version), kind_id, d.shape[0], d.shape[1],
                                       grid.shape[0], grid.shape[1])
           + VSA_SCALARS.pack(*[float(v) for v in fields]))
    return (hdr + d.tobytes() + grid.tobytes()
            + struct.pack('<I', meta.size) + meta.tobytes())


def decode_vsa(buf: bytes) -> dict | None:
    """Decode a VSAD frame with NumPy only (the measurement block stays float32).

    Returns None for a different magic or a length that does not match the declared
    shape, so a truncated frame can never be read with the wrong stride.
    """
    head = 4 + VSA_HEADER.size + VSA_SCALARS.size
    if len(buf) < head or buf[:4] != MAGIC_VSA:
        return None
    version, kind_id, rows, cols, ideal_rows, ideal_cols = VSA_HEADER.unpack_from(buf, 4)
    # Validate every declared size against the buffer *before* reading: a truncated frame
    # must be refused, not read with a wrong stride.
    arrays_end = head + (rows * cols + ideal_rows * ideal_cols) * 4
    if len(buf) < arrays_end + 4:
        return None
    (meta_len,) = struct.unpack_from('<I', buf, arrays_end)
    if len(buf) != arrays_end + 4 + meta_len * 4:
        return None
    scalars = VSA_SCALARS.unpack_from(buf, 4 + VSA_HEADER.size)
    data = np.frombuffer(buf, dtype='<f4', count=rows * cols, offset=head).reshape(rows, cols)
    ideal = np.frombuffer(buf, dtype='<f4', count=ideal_rows * ideal_cols,
                         offset=head + rows * cols * 4).reshape(ideal_rows, ideal_cols)
    meta = np.frombuffer(buf, dtype='<f4', count=meta_len, offset=arrays_end + 4)
    return {
        'version': version,
        'kind': VSA_KINDS[kind_id] if kind_id < len(VSA_KINDS) else str(kind_id),
        'kind_id': kind_id,
        'data': data,
        'ideal': ideal,
        'symbol_rate_hz': scalars[0], 'cfo_hz': scalars[1], 'timing_samples': scalars[2],
        'evm_percent': scalars[3], 'snr_db': scalars[4],
        'measurements': {key: float(meta[i]) for i, key in enumerate(VSA_MEASURE_KEYS)
                         if i < meta_len},
    }


def encode_audio(seq: int, rate: int, pcm) -> bytes:
    """Mono PCM16 audio frame; seq == 0 tells the client to flush stale audio."""
    pcm = np.ascontiguousarray(pcm, dtype=np.int16)
    return MAGIC_AUDIO + AUDIO_HEADER.pack(int(seq), int(rate), pcm.size) + pcm.tobytes()


def parse_header(buf: bytes):
    if len(buf) < 16:
        return None
    magic = buf[:4]
    ver, points = struct.unpack_from('<II', buf, 4)
    sweep_ms, = struct.unpack_from('<f', buf, 12)
    return magic, ver, points, sweep_ms


def decode_powr(buf: bytes, points: int) -> np.ndarray:
    return np.frombuffer(buf, dtype=np.float32, offset=16, count=points)
