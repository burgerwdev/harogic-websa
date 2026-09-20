"""Golden binary-frame fixtures: the wire format cannot change silently.

``tools/gen_frame_fixtures.py`` writes the fixtures from the production encoders; this
test re-encodes the same values and asserts the committed bytes still match. A format
change therefore fails here until the fixtures are regenerated, and the TypeScript test
(``frontend/modern/src/__tests__/frames.test.ts``) then proves the decoder still agrees.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.gen_frame_fixtures import build  # noqa: E402

FIXTURES = ROOT / 'tests' / 'fixtures' / 'frames'


@pytest.mark.parametrize('name', ['freq.bin', 'powr.bin', 'rta.bin', 'audio.bin', 'iq.bin'])
def test_committed_fixture_matches_the_encoder(name):
    files, _manifest = build()
    path = FIXTURES / name
    assert path.exists(), f'{name} is missing; run tools/gen_frame_fixtures.py'
    assert path.read_bytes() == files[name], (
        f'{name} no longer matches the production encoder; '
        'run python3 tools/gen_frame_fixtures.py and update the TypeScript test if the format changed')


def test_manifest_is_in_sync():
    _files, manifest = build()
    path = FIXTURES / 'manifest.json'
    assert path.exists()
    assert json.loads(path.read_text()) == json.loads(json.dumps(manifest, sort_keys=True))


def test_freq_and_powr_layout():
    """Pin the documented header/strides (magic + version + points + sweep_ms)."""
    files, manifest = build()
    freq = files['freq.bin']
    assert freq[:4] == b'FREQ'
    assert int.from_bytes(freq[4:8], 'little') == manifest['freq']['version']
    assert int.from_bytes(freq[8:12], 'little') == manifest['freq']['points']
    assert np.frombuffer(freq, dtype='<f4', count=1, offset=12)[0] == manifest['freq']['sweep_ms']
    assert len(freq) == 16 + manifest['freq']['points'] * 8

    powr = files['powr.bin']
    assert powr[:4] == b'POWR'
    assert len(powr) == 16 + manifest['powr']['points'] * 4
    # POWR must stay float32 (the frontend reads it as f4) even when given float64 input.
    assert np.frombuffer(powr, dtype='<f4', offset=16).tolist() == manifest['powr']['power']


def test_iq_layout_keeps_the_payload_aligned():
    """IQDF: magic + ver + seq + samples + rate(f8) + centre(f8) + int16 I/Q pairs.

    The two f64 fields live *before* the payload so the int16 block starts at byte 32: a
    typed-array view on an odd offset throws in the browser, and the client must be able to
    read the samples without copying.
    """
    files, manifest = build()
    iq = files['iq.bin']
    iq_m = manifest['iq']
    assert iq[:4] == b'IQDF'
    ver, seq, samples = np.frombuffer(iq, dtype='<u4', count=3, offset=4)
    assert (int(ver), int(seq), int(samples)) == (iq_m['version'], iq_m['seq'], iq_m['samples'])
    rate, center = np.frombuffer(iq, dtype='<f8', count=2, offset=16)
    assert float(rate) == iq_m['rate']
    assert float(center) == iq_m['center_hz']
    assert len(iq) == 32 + iq_m['samples'] * 2 * 2
    assert np.frombuffer(iq, dtype='<i2', offset=32).tolist() == iq_m['iq']
