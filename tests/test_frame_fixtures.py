"""Golden binary-frame fixtures: the wire format cannot change silently.

``tools/fixtures/gen_frame_fixtures.py`` writes the fixtures from independent reference encoders; this
test re-encodes the same values and asserts the committed bytes still match. A format
change therefore fails here until the fixtures are regenerated, and the TypeScript test
(``frontend/src/__tests__/frames.test.ts``) then proves the decoder still agrees.
"""
from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

import numpy as np
import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.fixtures.gen_frame_fixtures import build  # noqa: E402

FIXTURES = ROOT / 'tests' / 'fixtures' / 'frames'


@pytest.mark.parametrize('name', ['freq.bin', 'powr.bin', 'rta.bin', 'rta2.bin', 'audio.bin', 'baseband.bin', 'auxiliary.bin'])
def test_committed_fixture_matches_the_encoder(name):
    files, _manifest = build()
    path = FIXTURES / name
    assert path.exists(), f'{name} is missing; run tools/fixtures/gen_frame_fixtures.py'
    assert path.read_bytes() == files[name], (
        f'{name} no longer matches the reference encoder; '
        'run python3 tools/fixtures/gen_frame_fixtures.py and update the TypeScript test if the format changed')


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


@pytest.mark.parametrize('kind,magic', [('baseband', b'IQBF'), ('auxiliary', b'IQAX')])
def test_baseband_layout_keeps_the_payload_aligned(kind, magic):
    """The v2 metadata keeps the float32 payload aligned at byte 56."""
    files, manifest = build()
    baseband = files[f'{kind}.bin']
    meta = manifest[kind]
    assert baseband[:4] == magic
    ver, seq, samples = np.frombuffer(baseband, dtype='<u4', count=3, offset=4)
    assert (int(ver), int(seq), int(samples)) == (meta['version'], meta['seq'], meta['samples'])
    rate, center = np.frombuffer(baseband, dtype='<f8', count=2, offset=16)
    assert float(rate) == meta['rate']
    assert float(center) == meta['center_hz']
    channel_id, epoch = np.frombuffer(baseband, dtype='<u4', count=2, offset=32)
    assert (int(channel_id), int(epoch)) == (meta['channel_id'], meta['epoch'])
    assert int(np.frombuffer(baseband, dtype='<u8', count=1, offset=40)[0]) == int(meta['input_index'])
    assert float(np.frombuffer(baseband, dtype='<f8', count=1, offset=48)[0]) == meta['delay_samples']
    assert len(baseband) == 56 + meta['samples'] * 2 * 4
    got = np.frombuffer(baseband, dtype='<f4', offset=56).tolist()
    assert got == pytest.approx(meta['iq'])


@pytest.mark.parametrize('encoder', ['encode_baseband', 'encode_aux'])
def test_baseband_rejects_torn_iq_and_nonempty_boundaries(encoder):
    from tools.reference import framer

    encode = getattr(framer, encoder)
    metadata = dict(channel_id=3, epoch=7, input_index=2**53 + 1, delay_samples=1.25)
    with pytest.raises(ValueError, match='complete I/Q pairs'):
        encode(1, 48000.0, 100e6, [0.5], **metadata)
    with pytest.raises(ValueError, match='empty boundary'):
        encode(0, 48000.0, 100e6, [0.5, -0.5], **metadata)
    with pytest.raises(TypeError):
        encode(1, 48000.0, 100e6, [0.5, -0.5])


def test_rta2_layout_and_packet_metadata():
    files, manifest = build()
    frame = files['rta2.bin']
    legacy = files['rta.bin']
    meta = manifest['rta2']
    end = 32 + 12 * meta['points'] + 2 * meta['wf_len']
    assert frame[:4] == b'RTA2'
    assert frame[4:end] == legacy[4:]
    assert len(legacy) == end
    assert struct.unpack_from('<IIHHd', frame, 4) == (
        meta['version'], meta['points'], meta['wf_len'], meta['max_density'], meta['start_hz'])
    assert struct.unpack_from('<d', frame, end - 8)[0] == meta['stop_hz']
    assert frame[end:end + 4] == b'TRIG'
    length = struct.unpack_from('<I', frame, end + 4)[0]
    assert len(frame) == end + 8 + length
    assert json.loads(frame[end + 8:]) == meta['metadata']


def test_rta2_nullable_packet_fields_and_utf8_length():
    from tools.reference.framer import encode_rta, encode_rta2

    metadata = dict(source=None, generation=0, frames=0, capture_id=None,
                    packet_index=None, packet_count=None, waiting=None,
                    complete=None, simulated=True, supported=False)
    args = (1, [100.0], [-30.0], [], 0, 90.0, 110.0)
    frame = encode_rta2(*args, metadata=metadata)
    legacy = encode_rta(*args)
    end = len(legacy)
    assert frame[4:end] == legacy[4:]
    assert struct.unpack_from('<I', frame, end + 4)[0] == len(frame[end + 8:])
    assert json.loads(frame[end + 8:]) == metadata
    # JSON length counts encoded bytes, including a non-ASCII SDK enum string.
    metadata['source'] = '外部'
    frame = encode_rta2(*args, metadata=metadata)
    assert struct.unpack_from('<I', frame, end + 4)[0] == len(frame[end + 8:])
    assert json.loads(frame[end + 8:]) == metadata
