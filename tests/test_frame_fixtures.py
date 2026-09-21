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


@pytest.mark.parametrize('name', ['freq.bin', 'powr.bin', 'rta.bin', 'audio.bin', 'baseband.bin'])
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


def test_baseband_layout_keeps_the_payload_aligned():
    """IQBF: magic + ver + seq + samples + rate(f8) + centre(f8) + float32 I/Q pairs.

    The two f64 fields live *before* the payload so the f32 block starts at byte 32: a typed-array
    view on an unaligned offset throws in the browser, and the demodulator must be able to read the
    samples without copying.
    """
    files, manifest = build()
    baseband = files['baseband.bin']
    meta = manifest['baseband']
    assert baseband[:4] == b'IQBF'
    ver, seq, samples = np.frombuffer(baseband, dtype='<u4', count=3, offset=4)
    assert (int(ver), int(seq), int(samples)) == (meta['version'], meta['seq'], meta['samples'])
    rate, center = np.frombuffer(baseband, dtype='<f8', count=2, offset=16)
    assert float(rate) == meta['rate']
    assert float(center) == meta['center_hz']
    assert len(baseband) == 32 + meta['samples'] * 2 * 4
    got = np.frombuffer(baseband, dtype='<f4', offset=32).tolist()
    assert got == pytest.approx(meta['iq'])
