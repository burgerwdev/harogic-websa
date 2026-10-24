#!/usr/bin/env python3
"""Compare native receiver PCM with the Python analog reference on captured float32 IQ.

    python3 tools/bench/hil_reference_check.py /tmp/hil_iq.json

The native side invokes native-ddc/target/release/sdr_decode. Build that executable
before running this tool, or select an existing executable with --native-bin.
Both sides use the capture's actual IQ cadence and report tone, SINAD and THD.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))
DEFAULT_NATIVE_BIN = ROOT / 'native-ddc' / 'target' / 'release' / 'sdr_decode'
AUDIO_RATE = 48_000


def load_iq(path: Path) -> tuple[dict, Path, np.ndarray]:
    meta = json.loads(path.read_text())
    iq_path = path.parent / meta['iq_file']
    if iq_path.stat().st_size % 8:
        raise ValueError(f'{iq_path}: expected interleaved float32 I,Q pairs')
    iq = np.fromfile(iq_path, dtype='<f4')
    if iq.size == 0 or iq.size // 2 != int(meta['samples']) or not np.isfinite(iq).all():
        raise ValueError(f'{iq_path}: empty, nonfinite or inconsistent capture')
    rate = float(meta['fs_in'])
    if not np.isfinite(rate) or rate <= 0:
        raise ValueError('capture fs_in must be finite and positive')
    return meta, iq_path, iq


def run_native(path: Path, native_bin: Path = DEFAULT_NATIVE_BIN) -> tuple[np.ndarray, int, dict]:
    """Replay the captured channel IQ through the production native Receiver."""
    meta, iq_path, _iq = load_iq(path)
    executable = native_bin.expanduser().resolve()
    if not executable.is_file():
        raise FileNotFoundError(f'native receiver executable missing: {executable}; build sdr_decode first')
    with tempfile.TemporaryDirectory(prefix='websa-native-replay-') as directory:
        pcm_path = Path(directory) / 'audio.f32'
        report_path = Path(directory) / 'report.json'
        subprocess.run([
            str(executable), '--input', str(iq_path.resolve()), '--mode', meta['mode'],
            '--input-rate', str(meta['fs_in']), '--bandwidth', str(meta['if_bw']),
            '--pitch', str(meta.get('pitch', 700.0)),
            '--audio-rate', str(int(meta.get('out_rate', AUDIO_RATE))),
            '--first-sample-index', str(int(meta.get('first_sample_index', 0))),
            '--delay-input-samples', str(float(meta.get('delay_input_samples', 0.0))),
            '--output-pcm', str(pcm_path), '--report-json', str(report_path),
        ], check=True)
        report = json.loads(report_path.read_text())
        formats = report['formats']
        if len(formats) != 1:
            raise ValueError(f'capture must produce one PCM format, got {formats}')
        fmt = formats[0]
        rate, channels = int(fmt['sample_rate']), int(fmt['channels'])
        if rate <= 0 or channels not in (1, 2) or pcm_path.stat().st_size % 4:
            raise ValueError(f'invalid native PCM format: {fmt}')
        pcm = np.fromfile(pcm_path, dtype='<f4')
        if (pcm.size != int(fmt['samples']) or pcm.size != int(fmt['frames']) * channels
                or pcm.size == 0 or not np.isfinite(pcm).all()):
            raise ValueError('native PCM is empty, nonfinite or disagrees with report counts')
        if not isinstance(report['events'], list):
            raise ValueError('native report events must be an array')
        return pcm.reshape(-1, channels), rate, report


def tone_measure(audio: np.ndarray, rate: float, nominal_hz: float) -> tuple[float, float, float]:
    """Return fitted tone frequency, peak amplitude and SINAD over a bounded window."""
    n = min(16_384, audio.size)
    if n < 4096 or not np.isfinite(audio).all() or not 0 < nominal_hz < rate / 2:
        raise ValueError('tone measurement requires 4096 finite samples and a tone below Nyquist')
    segment = audio[-n:].astype(np.float64)
    segment -= np.mean(segment)
    time_axis = np.arange(n) / rate
    window = np.hanning(n)
    weighted = segment * window

    def amplitude(hz: float) -> float:
        return float(2 * abs(np.sum(weighted * np.exp(-2j * np.pi * hz * time_axis))) / window.sum())

    low = max(1.0, nominal_hz * 0.7)
    high = min(rate / 2 - 1.0, nominal_hz * 1.3)
    best_hz = max(np.arange(low, high, 1.0), key=amplitude)
    best_hz = max(np.arange(max(low, best_hz - 1.0), min(high, best_hz + 1.0), 0.02), key=amplitude)
    phase = 2 * np.pi * best_hz * time_axis
    basis = np.column_stack((np.sin(phase), np.cos(phase), np.ones(n)))
    fitted, *_ = np.linalg.lstsq(basis, segment, rcond=None)
    tone = basis[:, :2] @ fitted[:2]
    tone_power = float(np.mean(tone ** 2))
    if tone_power <= 1e-20:
        raise ValueError('no measurable tone in capture')
    noise_power = max(float(np.mean((segment - basis @ fitted) ** 2)), 1e-20)
    return float(best_hz), float(np.hypot(*fitted[:2])), float(10 * np.log10(tone_power / noise_power))


def measure_audio(label: str, audio: np.ndarray, rate: int, expected: float) -> dict:
    if audio.ndim == 1:
        audio = audio[:, None]
    results = []
    for channel in range(audio.shape[1]):
        samples = audio[:, channel]
        tone_hz, tone_amp, sinad = tone_measure(samples, rate, expected)
        # Measure harmonics at their expected frequencies, not an overlapping fundamental search band.
        segment = samples[-min(16_384, samples.size):].astype(np.float64)
        window = np.hanning(segment.size)
        time_axis = np.arange(segment.size) / rate
        harmonics = [float(2 * abs(np.sum(segment * window * np.exp(-2j * np.pi * tone_hz * n * time_axis)))
                           / window.sum()) for n in (2, 3, 4) if tone_hz * n < rate / 2]
        thd = float(20 * np.log10(max(np.sqrt(sum(h * h for h in harmonics)), 1e-20) / tone_amp))
        level = float(20 * np.log10(max(float(np.sqrt(np.mean(samples.astype(np.float64) ** 2))), 1e-20)))
        result = {'tone_hz': tone_hz, 'sinad_db': sinad, 'thd_db': thd, 'level_dbfs': level}
        results.append(result)
        print(f'{label} channel {channel + 1}: {rate} Hz, tone {tone_hz:.2f} Hz '
              f'(nominal {expected:.0f}), level {level:.1f} dBFS, SINAD {sinad:.1f} dB, THD {thd:.1f} dB')
    return {'channels': results}


def python_reference(meta: dict, iq: np.ndarray) -> np.ndarray:
    from tools.reference.demod import AnalogDemod

    rate = int(meta.get('out_rate', AUDIO_RATE))
    demod = AnalogDemod(audio_rate=rate)
    # IQBF is already channelized; applying a second DDC changes the measurement bandwidth.
    demod.configure(fs=float(meta['fs_in']), mode=meta['mode'], if_bw=float(meta['if_bw']),
                    pitch=float(meta.get('pitch', 700.0)))
    blocks = []
    for start in range(0, iq.size, 8192):
        block = iq[start:start + 8192]
        audio, _power = demod.process(block[0::2], block[1::2])
        blocks.append(audio)
    return np.concatenate(blocks)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', nargs='?', type=Path, default=Path('/tmp/hil_iq.json'))
    parser.add_argument('--native-bin', type=Path, default=DEFAULT_NATIVE_BIN)
    args = parser.parse_args()
    meta, _iq_path, iq = load_iq(args.capture)
    expected = float(meta.get('expected_tone_hz') or (meta['pitch'] if meta['mode'] == 'cw' else 1000.0))
    native, native_rate, _report = run_native(args.capture, args.native_bin)
    measure_audio('native receiver', native, native_rate, expected)
    reference = python_reference(meta, iq)
    measure_audio('Python reference', reference, int(meta.get('out_rate', AUDIO_RATE)), expected)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
