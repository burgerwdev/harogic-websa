#!/usr/bin/env python3
"""Reference (ft8_lib) sensitivity baseline on scenarios equivalent to wasm ft8_snr_sweep.

Same fixture (ft8_cq_iq.bin, I channel = the audio), same in-band SNR convention (signal power
over noise power in 100-3000 Hz), same 4 frequency offsets x 2 noise seeds per level. Writes
48 kHz s16 WAVs and runs tools/ft8_reference/build.sh (time_osr/freq_osr as argv).
"""
import subprocess
import sys
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
RATE = 48_000.0
BAND = 3_000.0 - 100.0
SLOT = int(RATE * 15.0)
INSERT_S = 0.64
LEVELS = [-24.0, -22.0, -20.0, -18.0, -16.0, -14.0]
OFFSETS = [0.0, -3.125, 4.5, -11.25]
SEEDS = [0x5EED_2026, 0xBEEF_2026]
TIME_OSR = sys.argv[1] if len(sys.argv) > 1 else '4'
FREQ_OSR = sys.argv[2] if len(sys.argv) > 2 else '4'

iq = np.fromfile(ROOT / 'tests/fixtures/ft8/ft8_cq_iq.bin', dtype='<f4').reshape(-1, 2)
signal_i = iq[:, 0].astype(np.float64)          # zero-IF: I is the audio, power 0.5
signal_q = iq[:, 1].astype(np.float64)
n_sig = len(signal_i)
t = np.arange(n_sig) / RATE

for level in LEVELS:
    decoded = 0
    trials = 0
    for trial in range(8):
        offset = OFFSETS[trial % 4]
        seed = SEEDS[trial // 4]
        ph = np.exp(2j * np.pi * offset * t)
        shifted_i = signal_i * np.cos(2*np.pi*offset*t) - signal_q * np.sin(2*np.pi*offset*t)
        shifted_q = signal_i * np.sin(2*np.pi*offset*t) + signal_q * np.cos(2*np.pi*offset*t)
        # complex signal power stays ~1.0; I-channel audio power ~0.5
        audio = np.zeros(SLOT)
        at = int(INSERT_S * RATE)
        audio[at:at+n_sig] += shifted_i
        sig_power = float(np.mean(shifted_i**2))   # in-band audio power of the signal
        rng = np.random.default_rng(seed)
        sigma = np.sqrt(sig_power * 10**(-level/10.0) * RATE / BAND)
        noisy = audio + rng.normal(0.0, sigma, SLOT)
        pcm = np.clip(noisy / np.max(np.abs(noisy)) * 32000, -32768, 32767).astype('<i2')
        wav_path = Path('/tmp/ft8_ref_snr.wav')
        with wave.open(str(wav_path), 'wb') as w:
            w.setnchannels(1); w.setsampwidth(2); w.setframerate(int(RATE))
            w.writeframes(pcm.tobytes())
        proc = subprocess.run(
            ['bash', str(ROOT / 'tools/ft8_reference/build.sh'), str(wav_path), TIME_OSR, FREQ_OSR, '48000'],
            capture_output=True, text=True, check=True)
        ok = 'DECODED: CQ JO1WKO' in proc.stdout
        decoded += ok
        trials += 1
    print(f"  {level:>5.0f} dB | ref {decoded}/{trials}")
