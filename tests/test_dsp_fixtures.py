"""DSP golden fixtures: the Rust/WASM kernels' reference output cannot change silently.

``tools/gen_dsp_fixtures.py`` runs the Python reference DSP over a committed IQ block and writes
the per-stage results the Rust tests in ``wasm/tests/ddc_reference.rs`` are compared against
(within the tolerance recorded in the manifest). This test re-runs the reference and asserts the
committed bytes still match, so changing the Python DSP forces a deliberate fixture update — and
the Rust test then proves the two implementations still agree.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.gen_dsp_fixtures import build  # noqa: E402

FIXTURES = ROOT / 'tests' / 'fixtures' / 'dsp'


def test_committed_dsp_fixtures_match_the_reference():
    files, _manifest = build()
    problems = [name for name, data in files.items()
                if not (FIXTURES / name).exists() or (FIXTURES / name).read_bytes() != data]
    assert not problems, (
        f'{problems} no longer match the Python reference DSP; run '
        'python3 tools/gen_dsp_fixtures.py (and keep the Rust tests in step)')


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
