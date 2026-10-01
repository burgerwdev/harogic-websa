"""DSP golden fixtures: the Rust/WASM kernels' reference output cannot change silently.

``tools/gen_dsp_fixtures.py`` runs the Python reference DSP over a committed IQ block and writes
the per-stage results the Rust tests in ``wasm/tests/ddc_reference.rs`` are compared against
(within the tolerance recorded in the manifest). This test re-runs the reference and asserts the
committed fixtures still match within that same tolerance, so changing the Python DSP forces a
deliberate fixture update - and the Rust test then proves the two implementations still agree.
Only the synthetic integer input and the manifest are compared exactly.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.gen_dsp_fixtures import build, drifted  # noqa: E402

FIXTURES = ROOT / 'tests' / 'fixtures' / 'dsp'


def test_committed_dsp_fixtures_match_the_reference():
    files, manifest = build()
    problems = []
    for name, data in files.items():
        path = FIXTURES / name
        problem = (f'{name}: missing' if not path.exists()
                   else drifted(name, path.read_bytes(), data, manifest))
        if problem:
            problems.append(problem)
    assert not problems, (
        f'{problems} no longer match the Python reference DSP; run '
        'python3 tools/gen_dsp_fixtures.py (and keep the Rust tests in step)')


def test_the_drift_check_absorbs_ulp_noise_and_catches_real_changes():
    """The guard must not become a no-op: one ULP passes, a real change does not.

    Byte equality was the previous rule and it failed CI for a few ULP of numpy-kernel
    difference; the manifest tolerance is the contract the Rust tests already honor.
    """
    _files, manifest = build()
    # A tight-tolerance case (usb grants 1e-5; the DC-blocked modes grant 0.002), so the
    # magnitudes below are unambiguous against the tolerance that actually applies.
    name = 'demod_usb.bin'
    committed = (FIXTURES / name).read_bytes()
    values = np.frombuffer(committed, dtype='<f4').copy()

    one_ulp = values.copy()
    one_ulp[0] = np.nextafter(one_ulp[0], np.float32(1.0))
    assert drifted(name, one_ulp.tobytes(), committed, manifest) is None

    real_change = values.copy()
    real_change[len(real_change) // 2] += np.float32(1e-3)     # 100x the tolerance
    assert drifted(name, real_change.tobytes(), committed, manifest) is not None

    # The synthetic integer input is not a float result: it must still match exactly.
    iq = FIXTURES / 'iq_block.bin'
    changed = bytearray(iq.read_bytes())
    changed[0] ^= 0x01
    assert drifted('iq_block.bin', bytes(changed), iq.read_bytes(), manifest) is not None


def test_manifest_is_in_sync():
    _files, manifest = build()
    committed = json.loads((FIXTURES / 'manifest.json').read_text())
    assert committed == json.loads(json.dumps(manifest, sort_keys=True))


def test_the_fixture_does_not_clip_and_the_wanted_tone_survives_the_mixer():
    """Cheap sanity on the fixture itself: a clipped or off-bin block measures intermodulation
    products instead of the tones it claims, which would make every comparison meaningless."""
    files, manifest = build()
    iq = np.frombuffer(files['iq_block.bin'], dtype='<i2')
    assert np.abs(iq.astype(np.float64) / 32768.0).max() < 1.0, 'the IQ block clips'
    checks = manifest['checks']
    assert checks['wanted_amp_after_mix'] == np.float32(checks['wanted_amp_after_mix'])
    assert 0.38 < checks['wanted_amp_after_mix'] < 0.42, checks['wanted_amp_after_mix']
    assert abs(checks['wanted_peak_after_mix_hz']) < 1.0, checks  # the tone lands on DC
    assert checks['interferer_suppression_db'] < -60.0, checks
    assert manifest['tolerance'] > 0.0
