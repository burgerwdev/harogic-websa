"""The analog transmit helpers: audio loading, length fitting, level and modulation.

These run without a radio: the helpers and the baseband builder are pure numpy.
"""
from __future__ import annotations

import argparse
import sys
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.pluto import pluto_analog_tx as analog  # noqa: E402


def _write_pcm24(path: Path, samples: np.ndarray, rate: int) -> None:
    ints = np.round(np.clip(samples, -1.0, 1.0) * (2 ** 23 - 1)).astype(np.int64)
    u = ints & 0xFFFFFF
    b = np.empty((ints.size, 3), dtype=np.uint8)
    b[:, 0] = u & 0xFF
    b[:, 1] = (u >> 8) & 0xFF
    b[:, 2] = (u >> 16) & 0xFF
    with wave.open(str(path), 'wb') as w:
        w.setnchannels(1)
        w.setsampwidth(3)
        w.setframerate(rate)
        w.writeframes(b.tobytes())


def _args(mode: str, **over) -> argparse.Namespace:
    base = dict(mode=mode, wav=None, channel='sum', duration=None, no_loop=False,
                normalize_peak=0.9, no_normalize=False, audio_gain_db=0.0,
                tone_hz=1000.0, am_depth=0.5, dsb_carrier=0.5, deviation_hz=3000.0,
                pm_index=1.0, base_hz=0.0, scale=1.0)
    base.update(over)
    return argparse.Namespace(**base)


def test_fit_length_keeps_loops_pads_and_truncates():
    a = np.arange(4, dtype=np.float32)
    assert analog.fit_length(a, 10, None, True).size == 4          # no duration: whole file
    assert analog.fit_length(a, 1, 10.0, True).size == 10          # loop to the length
    assert analog.fit_length(a, 1, 10.0, False).size == 10         # pad when not looping
    assert analog.fit_length(np.arange(100, dtype=np.float32), 1, 10.0, True).size == 10


def test_apply_level_normalises_the_peak():
    a = np.array([0.1, -0.2], dtype=np.float32)
    out = analog.apply_level(a, 0.9, 0.0, True)
    assert abs(float(np.max(np.abs(out))) - 0.9) < 1e-6
    assert np.array_equal(analog.apply_level(a, 0.9, 0.0, False), a)


def test_load_audio_reads_24_bit_and_picks_the_channel(tmp_path):
    rate = 16000
    t = np.arange(rate) / rate
    tone = 0.5 * np.cos(2 * np.pi * 440 * t)
    path = tmp_path / 't24.wav'
    _write_pcm24(path, tone, rate)
    audio = analog.load_audio(str(path), 48000.0)
    assert audio.size == 48000
    assert 0.49 < float(np.max(np.abs(audio))) < 0.52


def test_nfm_deviation_matches_the_setting():
    rate = 100000.0
    sig = analog.build_baseband(_args('nfm', deviation_hz=3000.0, duration=0.05), rate)
    phase = np.unwrap(np.angle(sig))
    inst = np.diff(phase) / (2 * np.pi) * rate
    mid = inst[inst.size // 4:-inst.size // 4]
    assert abs(float(np.max(mid)) - 3000.0) < 5.0


def test_usb_puts_the_tone_above_the_dial():
    rate = 100000.0
    sig = analog.build_baseband(_args('usb', tone_hz=1000.0, duration=0.05), rate)
    n = sig.size
    spectrum = np.abs(np.fft.fftshift(np.fft.fft(sig))) ** 2
    freqs = np.fft.fftshift(np.fft.fftfreq(n, 1.0 / rate))
    assert abs(float(freqs[np.argmax(spectrum)]) - 1000.0) < 5.0
    assert spectrum[freqs < -50].sum() < spectrum[freqs > 50].sum() * 1e-3
