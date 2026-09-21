#!/usr/bin/env python3
"""Generate the DDC golden fixtures the Rust/WASM kernels are compared against.

The DSP moved to the browser, but the *numerics* did not get to change: the Python
implementations in `web_sa/demod/filters.py` (design_lowpass, StreamFilter, LinearResampler,
Agc) are the reference the Rust kernels must agree with, and this script turns that reference
into committed bytes.

It writes, into `tests/fixtures/dsp/`:

  iq_block.bin    the input: interleaved int16 IQ at 1 MSps (8192 complex samples), made of a
                  wanted tone that the NCO drops to DC, an out-of-band interferer, and a
                  deterministic noise floor
  mix.bin         reference complex f32 after the NCO mix
  fir.bin         reference complex f32 after the anti-alias FIR
  dec.bin         reference complex f32 after integer decimation
  res.bin         reference complex f32 after the linear resampler
  agc.bin         reference real f32 after the RMS AGC
  manifest.json   parameters, shapes and the tolerance the Rust test asserts

The chain matches the architecture: NCO -> FIR -> decimate -> resample -> level. Each stage is
dumped separately so a Rust failure names the stage that drifted instead of "the DDC".

Usage:  python3 tools/gen_dsp_fixtures.py [--check]
        --check  fail when the files on disk differ from what this script would write
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import NamedTuple

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from web_sa.demod.demod import AGC_REFERENCE  # noqa: E402
from web_sa.demod.filters import (  # noqa: E402
    Agc,
    LinearResampler,
    StreamFilter,
    design_lowpass,
)

OUT = ROOT / 'tests' / 'fixtures' / 'dsp'

FS_IN = 1_000_000.0         # IQS-style input rate (1 MSps keeps the fixture small)
SAMPLES = 8192              # complex samples
#: Tones are placed exactly on FFT bins (fs_in/samples) so the manifest's sanity checks are
#: meaningful: an off-bin tone leaks through the Hann window and the measured amplitude is not
#: the tone amplitude.
BIN_HZ = FS_IN / SAMPLES
WANTED_HZ = 656 * BIN_HZ        # 80078.125 Hz: the signal of interest
INTERFERER_HZ = 2868 * BIN_HZ   # 350097.65625 Hz: outside the decimated band, must be filtered
#: Keep the sum below full scale: I and Q are clipped independently at +/-1.0, and a clipped
#: fixture measures intermodulation products instead of the tones that were asked for.
SIGNAL_AMP = 0.4
INTERFERER_AMP = 0.3
NOISE_SIGMA = 1.0e-3
DECIMATE = 10               # fs_dec = 100 kHz
CUTOFF_HZ = 40_000.0        # 0.8 * fs_dec/2, the anti-alias corner
NTAPS = 129
OUT_RATE = 48_000.0         # channel/output rate of the resampler stage
OFFSET_HZ = WANTED_HZ       # NCO offset: mixing by -offset_hz brings the wanted tone to DC
#: Absolute tolerance on the f32 stage outputs (full scale 1.0). Measured worst case is ~1e-6
#: for the f64 kernels; 1e-5 leaves room for libm/numpy differences in cos/sin/sinc without
#: making the test meaningless (1e-5 is about -100 dBFS).
TOLERANCE = 1.0e-5


def build_iq() -> np.ndarray:
    """Deterministic interleaved int16 IQ block."""
    rng = np.random.default_rng(7)
    n = np.arange(SAMPLES)
    wanted = SIGNAL_AMP * np.exp(2j * np.pi * WANTED_HZ / FS_IN * n)
    # The offset has to sit inside the phase: `exp(2j*pi*f*n/fs + 0.7)` would scale the
    # amplitude by e**0.7 (= 2.01) and silently push the block into clipping.
    interferer = INTERFERER_AMP * np.exp(1j * (2 * np.pi * INTERFERER_HZ / FS_IN * n + 0.7))
    noise = (rng.normal(0.0, NOISE_SIGMA, SAMPLES)
             + 1j * rng.normal(0.0, NOISE_SIGMA, SAMPLES))
    x = wanted + interferer + noise
    peak = float(np.max(np.abs(x)))
    if peak >= 1.0:
        raise SystemExit(f'fixture would clip: peak |x| = {peak:.4f} >= 1.0 full scale; '
                         'lower SIGNAL_AMP/INTERFERER_AMP')
    i = np.round(x.real * 32767.0).astype(np.int16)
    q = np.round(x.imag * 32767.0).astype(np.int16)
    return np.stack([i, q], axis=1).reshape(-1)


def reference_chain(iq: np.ndarray):
    """The Python reference chain, one return value per stage."""
    z = (iq[0::2].astype(np.float64) + 1j * iq[1::2].astype(np.float64)) / 32768.0

    inc = -2.0 * np.pi * OFFSET_HZ / FS_IN
    mixed = z * np.exp(1j * (inc * np.arange(z.size)))

    h = design_lowpass(FS_IN, CUTOFF_HZ, ntaps=NTAPS)
    filtered = StreamFilter(h).process(mixed)

    decimated = filtered[::DECIMATE]

    # One resampler per component. LinearResampler's phase depends only on the input length, so
    # the two stay in lockstep; the Rust side wraps the same two in one object so callers cannot
    # configure them differently.
    fs_dec = FS_IN / DECIMATE
    re = LinearResampler(fs_dec, OUT_RATE).process(decimated.real.astype(np.float32))
    im = LinearResampler(fs_dec, OUT_RATE).process(decimated.imag.astype(np.float32))
    resampled = re.astype(np.float64) + 1j * im.astype(np.float64)

    agc = Agc(**AGC_REFERENCE)
    levelled = agc.process(resampled.real.astype(np.float32))
    return mixed, filtered, decimated, resampled, levelled


def _complex_bytes(values: np.ndarray) -> bytes:
    out = np.empty(values.size * 2, dtype=np.float32)
    out[0::2] = values.real.astype(np.float32)
    out[1::2] = values.imag.astype(np.float32)
    return out.tobytes()


def _peak_hz(values: np.ndarray, rate: float) -> float:
    """Signed frequency of the strongest bin of a complex or real block."""
    if values.size < 8:
        return 0.0
    window = np.hanning(values.size)
    spectrum = np.abs(np.fft.fftshift(np.fft.fft(values * window)))
    freqs = np.fft.fftshift(np.fft.fftfreq(values.size, 1.0 / rate))
    return float(freqs[int(np.argmax(spectrum))])


#: Analog demodulator cross-check: complex baseband in, audio out, both sides of the contract.
DEMOD_RATE = 96_000.0
DEMOD_SAMPLES = 9_600          # 100 ms
DEMOD_AUDIO_RATE = 48_000.0


class DemodCase(NamedTuple):
    mode: str
    if_bw: float
    pitch: float
    expect_hz: float
    #: Bound on the absolute difference against the reference for this mode.
    #:
    #: Modes whose detector is followed by the one-pole DC blocker are bounded two orders looser on
    #: purpose: that blocker's noise gain is 1/(1-0.9995) = 2000, so the ~1e-7 difference between the
    #: Rust FIR and numpy's convolution (which the modes *without* a blocker show directly, at 1.8e-7)
    #: is amplified to ~1e-3 here. The measured worst for those modes is 7.4e-4; the bound is set just
    #: above it and the reason is recorded rather than hidden in a global constant.
    tolerance: float = 1.0e-5


#: Every analog mode the registry declares: the parity harness has to cover the whole family, not
#: the three modes that happened to be written first.
DEMOD_CASES: tuple[DemodCase, ...] = (
    DemodCase('am', 12_000.0, 700.0, 1_000.0, tolerance=2.0e-3),
    DemodCase('dsb', 12_000.0, 700.0, 1_000.0, tolerance=2.0e-3),
    DemodCase('usb', 2_400.0, 700.0, 1_200.0),
    DemodCase('lsb', 2_400.0, 700.0, 1_200.0),
    DemodCase('cw', 500.0, 700.0, 700.0),
    DemodCase('nfm', 12_000.0, 700.0, 1_000.0),
    DemodCase('wfm', 180_000.0, 700.0, 1_000.0),
    DemodCase('pm', 12_000.0, 700.0, 1_000.0, tolerance=2.0e-3),
)


def _demod_baseband(mode: str) -> np.ndarray:
    """The baseband a given mode is meant to receive, one case per family behaviour."""
    t = np.arange(DEMOD_SAMPLES) / DEMOD_RATE
    if mode == 'am':
        # Carrier with a 1 kHz modulation; the envelope detector must recover the tone.
        return (1.0 + 0.5 * np.cos(2 * np.pi * 1_000.0 * t)) + 0j
    if mode == 'dsb':
        # DSB with a residual carrier (a fully suppressed carrier has no envelope to detect).
        return (0.8 + 0.5 * np.cos(2 * np.pi * 1_000.0 * t)) + 0j
    if mode == 'usb':
        return 0.5 * np.exp(2j * np.pi * 1_200.0 * t)
    if mode == 'lsb':
        return 0.5 * np.exp(2j * np.pi * -1_200.0 * t)
    if mode == 'cw':
        # The carrier the operator tuned onto, plus an interferer 3 kHz away.
        return 1.0 + 0.5 * np.exp(2j * np.pi * 3_000.0 * t)
    if mode == 'nfm':
        return np.exp(1j * (1.0 * np.sin(2 * np.pi * 1_000.0 * t)))
    if mode == 'wfm':
        # Broadcast-like deviation, so the de-emphasis stage is exercised as well.
        return np.exp(1j * (3.0 * np.sin(2 * np.pi * 1_000.0 * t)))
    if mode == 'pm':
        return np.exp(1j * (0.6 * np.cos(2 * np.pi * 1_000.0 * t)))
    raise SystemExit(f'no fixture signal defined for mode {mode!r}')


#: Python modes that do not exist in `web_sa/demod` and how their reference is produced:
#:   dsb -> the `am` branch (DSB with a residual carrier is detected by the same envelope + DC block)
#:   pm  -> a phase detector assembled here from the same primitives, because the Python module has
#:          no PM kernel and the browser port must still be compared against something independent
#:          (adding PM to the production Python DSP would violate the "no new DSP in Python" rule).
REFERENCE_ALIAS = {'dsb': 'am'}


def _pm_reference(baseband: np.ndarray, if_bw: float) -> np.ndarray:
    """PM reference from the project's own DSP primitives (phase, DC block, LPF, resample, AGC)."""
    from web_sa.demod.filters import (
        Agc,
        LinearResampler,
        StreamFilter,
        design_complex_bandpass,
        design_lowpass,
        one_pole_iir,
    )

    band = StreamFilter(design_complex_bandpass(DEMOD_RATE, -if_bw / 2.0, if_bw / 2.0, ntaps=257))
    filtered = band.process(baseband)
    phase = np.angle(filtered).astype(np.float32)
    a = 0.9995
    delta = np.empty_like(phase)
    delta[0] = phase[0]
    delta[1:] = np.diff(phase)
    delta *= a
    blocked, _state = one_pole_iir(delta, a, 0.0)
    audio_lp = StreamFilter(design_lowpass(DEMOD_RATE, min(15_000.0, if_bw / 2.0), ntaps=129))
    audio = audio_lp.process(blocked.astype(np.float32))
    audio = LinearResampler(DEMOD_RATE, DEMOD_AUDIO_RATE).process(audio)
    return Agc(**AGC_REFERENCE).process(audio)


def build_demod_reference() -> tuple[dict[str, bytes], list[dict]]:
    """Python `AnalogDemod` output for each demodulator case, as committed bytes."""
    from web_sa.demod.demod import AnalogDemod

    files: dict[str, bytes] = {}
    entries: list[dict] = []
    for case in DEMOD_CASES:
        baseband = _demod_baseband(case.mode)
        if case.mode == 'pm':
            audio = _pm_reference(baseband, case.if_bw)
            reference = 'pm-reference (primitives assembled in this tool)'
            demod_audio = audio
        else:
            demod = AnalogDemod(audio_rate=DEMOD_AUDIO_RATE)
            python_mode = REFERENCE_ALIAS.get(case.mode, case.mode)
            demod.configure(fs=DEMOD_RATE, mode=python_mode, if_bw=case.if_bw, pitch=case.pitch)
            demod_audio, _power = demod.process(baseband.real.copy(), baseband.imag.copy())
            reference = f'AnalogDemod({python_mode})'
        audio = np.asarray(demod_audio)
        iq_name = f'demod_{case.mode}_iq.bin'
        audio_name = f'demod_{case.mode}.bin'
        files[iq_name] = _complex_bytes(baseband)
        files[audio_name] = np.ascontiguousarray(audio, dtype=np.float32).tobytes()
        entries.append({
            'mode': case.mode, 'if_bw': case.if_bw, 'pitch': case.pitch,
            'reference': reference, 'tolerance': case.tolerance,
            'expect_hz': case.expect_hz, 'fs': DEMOD_RATE, 'audio_rate': DEMOD_AUDIO_RATE,
            'samples': DEMOD_SAMPLES, 'iq_file': iq_name, 'audio_file': audio_name,
            'audio_samples': int(audio.size),
        })
    return files, entries


def _tone_amp(values: np.ndarray, rate: float, hz: float) -> float:
    """Hann-windowed DFT amplitude at one frequency (coherent gain corrected)."""
    n = np.arange(values.size)
    window = np.hanning(values.size)
    return float(abs(np.sum(values * window * np.exp(-2j * np.pi * hz / rate * n)))
                 / float(window.sum()))


def build() -> tuple[dict[str, bytes], dict]:
    iq = build_iq()
    mixed, filtered, decimated, resampled, levelled = reference_chain(iq)
    files = {
        'iq_block.bin': iq.tobytes(),
        'mix.bin': _complex_bytes(mixed),
        'fir.bin': _complex_bytes(filtered),
        'dec.bin': _complex_bytes(decimated),
        'res.bin': _complex_bytes(resampled),
        'agc.bin': levelled.astype(np.float32).tobytes(),
    }
    # ---- analog demodulator cross-check ----
    demod_files, demod_entries = build_demod_reference()
    files.update(demod_files)
    manifest = {
        'note': 'Generated by tools/gen_dsp_fixtures.py from the Python reference DSP '
                '(web_sa/demod/filters.py). Do not edit by hand. The Rust kernels in wasm/ '
                'are compared against these bytes.',
        'tolerance': TOLERANCE,
        'tolerance_note': 'Absolute difference allowed on each f32 stage output (full scale 1.0).',
        'fs_in': FS_IN,
        'samples': SAMPLES,
        'peak_abs': float(np.max(np.abs(iq.astype(np.float64) / 32768.0))),
        'decimate': DECIMATE,
        'out_rate': OUT_RATE,
        'cutoff_hz': CUTOFF_HZ,
        'ntaps': NTAPS,
        'offset_hz': OFFSET_HZ,
        'wanted_hz': WANTED_HZ,
        'interferer_hz': INTERFERER_HZ,
        'stages': [
            {'name': 'mix', 'file': 'mix.bin', 'kind': 'complex', 'length': int(mixed.size)},
            {'name': 'fir', 'file': 'fir.bin', 'kind': 'complex', 'length': int(filtered.size)},
            {'name': 'dec', 'file': 'dec.bin', 'kind': 'complex', 'length': int(decimated.size)},
            {'name': 'res', 'file': 'res.bin', 'kind': 'complex', 'length': int(resampled.size)},
            {'name': 'agc', 'file': 'agc.bin', 'kind': 'real', 'length': int(levelled.size)},
        ],
        'demod': demod_entries,
        # Readable expectations: the wanted tone sits at DC after the mix, and the FIR rejects the
        # interferer (so the decimated block is dominated by the wanted signal).
        'checks': {
            'wanted_peak_after_mix_hz': _peak_hz(mixed, FS_IN),
            'wanted_amp_after_mix': _tone_amp(mixed, FS_IN, 0.0),
            'peak_after_resample_hz': _peak_hz(resampled, OUT_RATE),
            'interferer_suppression_db': float(20.0 * np.log10(
                _tone_amp(filtered, FS_IN, INTERFERER_HZ - OFFSET_HZ)
                / (INTERFERER_AMP + 1e-12) + 1e-12)),
        },
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
        problems = []
        for name, data in files.items():
            path = OUT / name
            if not path.exists() or path.read_bytes() != data:
                problems.append(name)
        if not (OUT / 'manifest.json').exists() or (OUT / 'manifest.json').read_bytes() != manifest_bytes:
            problems.append('manifest.json')
        if problems:
            print('dsp fixture drift:', ', '.join(problems), file=sys.stderr)
            print('run: python3 tools/gen_dsp_fixtures.py', file=sys.stderr)
            return 1
        print(f'dsp fixtures OK: {len(files)} files, {manifest["samples"]} complex samples')
        return 0

    OUT.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (OUT / name).write_bytes(data)
        print(f'{name}: {len(data)} bytes')
    (OUT / 'manifest.json').write_bytes(manifest_bytes)
    print('manifest.json')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
