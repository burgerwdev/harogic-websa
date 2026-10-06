#!/usr/bin/env python3
"""Run the *Python reference* DSP over a hardware capture, for comparison with the browser kernels.

`tools/bench/hil_audio_check.py` captures real IQ; `frontend/src/__tests__/hil.test.ts` measures it
through the committed `dsp.wasm`. This tool measures the same capture with the Python chain the
browser kernels were ported from (`web_sa/demod/filters.py` + `demod/demod.py`), so a disappointing
number on the bench can be attributed: if the reference shows the same SINAD, the limit is the
hardware (a TinySA and an analyzer synthesizer have finite phase noise, and an FM/CW demodulator
turns exactly that into audio noise); if the reference is much better, the browser chain has a bug.

    python3 tools/bench/hil_reference_check.py /tmp/hil_iq.json
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from web_sa.demod.demod import AnalogDemod  # noqa: E402
from web_sa.demod.filters import LinearResampler, StreamFilter, design_lowpass  # noqa: E402

AUDIO_RATE = 48_000.0


def tone_measure(audio: np.ndarray, rate: float, nominal_hz: float) -> tuple[float, float, float]:
    """(tone_hz, tone_amplitude, sinad_db) over a bounded window, with a band search.

    The window is bounded on purpose: a Hann-windowed DFT over millions of samples has a main lobe
    far narrower than any practical search step, so probing "the peak" slightly off reads the tone as
    absent. 16384 samples at 48 kHz has a ~2.9 Hz bin, which the search resolution tolerates.
    """
    n = min(16_384, audio.size)
    segment = audio[-n:]
    window = np.hanning(n)

    def amplitude(hz: float) -> float:
        ph = 2 * np.pi * hz * np.arange(n) / rate
        return float(2 * abs(np.sum(segment * window * np.exp(-1j * ph))) / window.sum())

    coarse = max((amplitude(hz) for hz in np.arange(nominal_hz * 0.7, nominal_hz * 1.3, 1.0)),
                 default=0.0)
    best_hz = nominal_hz
    best = coarse
    for hz in np.arange(nominal_hz * 0.7, nominal_hz * 1.3, 1.0):
        value = amplitude(float(hz))
        if value > best:
            best, best_hz = value, float(hz)
    for hz in np.arange(best_hz - 1.0, best_hz + 1.0, 0.1):
        value = amplitude(float(hz))
        if value > best:
            best, best_hz = value, float(hz)
    total = float(np.sqrt(np.mean(segment.astype(np.float64) ** 2)))
    tone_power = best * best / 2
    noise_power = max(total * total - tone_power, 1e-20)
    return best_hz, best, 10 * np.log10(tone_power / noise_power)


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else '/tmp/hil_iq.json')
    meta = json.loads(path.read_text())
    iq = np.frombuffer(path.with_suffix('.iq').read_bytes(), dtype='<i2')
    z = (iq[0::2].astype(np.float64) + 1j * iq[1::2].astype(np.float64)) / 32768.0

    fs_in = float(meta['fs_in'])
    out_rate = float(meta.get('out_rate', AUDIO_RATE))
    decimate = int(meta.get('decimate') or max(1, int(fs_in // (out_rate * 4))))
    # The same geometry the browser DDC uses: anti-alias low-pass, integer decimation, resampling.
    fs_dec = fs_in / decimate
    taps = design_lowpass(fs_in, fs_dec * 0.4, ntaps=129)
    filtered = StreamFilter(taps).process(z)
    decimated = filtered[::decimate]
    re = LinearResampler(fs_dec, out_rate).process(decimated.real.astype(np.float32))
    im = LinearResampler(fs_dec, out_rate).process(decimated.imag.astype(np.float32))

    demod = AnalogDemod(audio_rate=out_rate)
    demod.configure(fs=out_rate, mode=meta['mode'], if_bw=float(meta['if_bw']),
                    pitch=float(meta.get('pitch', 700.0)))
    audio, _power = demod.process(re.astype(np.float64), im.astype(np.float64))
    audio = np.asarray(audio, dtype=np.float64)
    if audio.size < 4096:
        print('reference produced too little audio to measure', file=sys.stderr)
        return 1

    expected = float(meta.get('expected_tone_hz') or (meta['pitch'] if meta['mode'] == 'cw' else 1000.0))
    tone_hz, tone_amp, sinad = tone_measure(audio, out_rate, expected)
    harmonics = [tone_measure(audio, out_rate, tone_hz * n)[1] for n in (2, 3, 4)]
    thd = 20 * np.log10(np.sqrt(sum(h * h for h in harmonics)) / max(tone_amp, 1e-12))
    level = 20 * np.log10(np.sqrt(np.mean(audio ** 2)) + 1e-12)

    print(f"python reference: {meta['mode']} @ {meta['center_hz']/1e6:.4f} MHz, "
          f"{int(meta['samples'])} complex samples at {fs_in/1e6:.4f} MSps")
    print(f"  tone {tone_hz:.1f} Hz (nominal {expected:.0f} Hz), level {level:.1f} dBFS, "
          f"SINAD {sinad:.1f} dB, THD {thd:.1f} dB")
    print('  compare with the WASM measurement printed by hil.test.ts')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
