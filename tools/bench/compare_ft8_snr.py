#!/usr/bin/env python3
"""Reproducible FT8 measurement comparison with independent WSJT-X executables.

Obtain jt9 and ft8sim from a trusted WSJT-X distribution, e.g. unpack Debian's
wsjtx_2.7.0+repack-1_amd64.deb into /tmp/wsjtx (no system installation needed).
Requires numpy. Outputs source hashes, exact IQ, reference reports and native CSV.

python3 tools/bench/compare_ft8_snr.py --reference-bin /tmp/wsjtx/usr/bin \
    --output /tmp/ft8-snr-reference
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
LEVELS = [-24, -20, -16, -10, 0, 8]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference-bin', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    executables = {name: (args.reference_bin / name).resolve() for name in ('jt9', 'ft8sim')}
    subprocess.run([str(executables['ft8sim']), 'CQ JO1WKO PM95', '1000.37', '0', '0', '0', '1', '99'],
                   cwd=output, check=True, capture_output=True, timeout=30)
    with wave.open(str(output / '000000_000001.wav')) as wav:
        rate = wav.getframerate()
        signal = np.frombuffer(wav.readframes(wav.getnframes()), '<i2').astype(np.float64) / 32768
    assert rate == 12000
    active = signal[int(.5*rate):int((.5+12.64)*rate)]
    signal_power = np.mean(active**2)
    rows = []
    for level in LEVELS:
        rng = np.random.default_rng(20261110)
        # Real AWGN one-sided density = 2*sigma²/Fs. Active sine power,
        # NOT its duty-cycle-reduced 15-second slot average, sets the SNR.
        sigma = np.sqrt(signal_power*rate/(2*2500*10**(level/10)))
        combined = signal + sigma*rng.normal(size=len(signal))
        combined *= .05 / np.max(abs(combined))
        pcm = np.round(combined*32768).astype('<i2')
        path = output / f'case_{level:+03d}.wav'
        with wave.open(str(path), 'w') as wav:
            wav.setnchannels(1); wav.setsampwidth(2); wav.setframerate(rate); wav.writeframes(pcm.tobytes())
        iq = np.zeros((len(pcm), 2), dtype='<f4'); iq[:, 0] = pcm.astype(float)/32768
        iq.tofile(output / f'case_{level:+03d}.iq')
        reference = subprocess.run([str(executables['jt9']), '-8', '-d', '3', '-t', str(output), '-a', str(output), str(path)],
                                   cwd=output, check=True, capture_output=True, text=True, timeout=45)
        match = re.search(r'^\s*\S+\s+(-?\d+)\s+.*CQ JO1WKO PM95', reference.stdout, re.M)
        measured = int(match[1]) if match else None
        if measured is not None:
            (output / f'case_{level:+03d}.reference').write_text(str(measured)+'\n')
        elif level >= -20:
            raise RuntimeError(f'WSJT-X did not decode {level} dB: {reference.stdout} {reference.stderr}')
        rows.append(dict(expected_db=level, wsjtx_db=measured, reference_output=reference.stdout,
                         iq_sha256=hashlib.sha256(iq.tobytes()).hexdigest()))
    (output/'reference-results.json').write_text(json.dumps(dict(
        seed=20261110, bandwidth_hz=2500, method='WSJT-X GFSK plus real AWGN; identical quantized input',
        executable_sha256={name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in executables.items()},
        rows=rows), indent=2)+'\n')
    env = {**os.environ, 'WEBSA_FT8_REFERENCE_DIR': str(output)}
    subprocess.run(['cargo', 'test', '--release', '-j', '1', '--manifest-path', str(ROOT/'dsp/core/Cargo.toml'),
                    '--test', 'ft8_snr_measurement', '--', '--ignored', '--nocapture'], cwd=ROOT, env=env, check=True)

if __name__ == '__main__':
    main()
