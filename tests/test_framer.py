"""framer 编解码 + dtype guard 单测"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pytest

from web_sa.measurements.framer import (
    MAGIC_VSA,
    VSA_MEASURE_KEYS,
    decode_powr,
    decode_vsa,
    encode_freq,
    encode_powr,
    encode_vsa,
    parse_header,
)


def test_powr_float32():
    p = np.array([-20.5, -30.25, -40.0], dtype=np.float32)
    fr = encode_powr(3, p, 0.01)
    m, v, pts, ms = parse_header(fr)
    assert (m, v, pts) == (b'POWR', 3, 3)
    assert np.allclose(decode_powr(fr, pts), p, atol=1e-5)


def test_freq_float64():
    f = np.linspace(950e6, 1050e6, 1001)
    fr = encode_freq(1, f, 0.02)
    m, v, pts, _ = parse_header(fr)
    assert m == b'FREQ' and pts == 1001
    assert np.allclose(np.frombuffer(fr, dtype=np.float64, offset=16, count=pts), f)


def test_float64_dtype_guard():
    """旧 bug: np.interp 把 float32 变 float64; encode 必须强制 float32"""
    x = np.linspace(0, 1, 1001)
    p64 = np.interp(x, x, np.linspace(-100, -20, 1001))
    fr = encode_powr(1, p64, 0.0)
    assert len(fr) == 16 + 1001 * 4


def test_vsa_round_trip_including_the_measurement_block():
    """VSAD: kind, cloud, ideal grid, the five scalars and the positional measurement block."""
    cloud = np.array([[0.001, -0.001], [-0.0005, 0.00025], [0.0, 0.0]], dtype=np.float32)
    ideal = np.array([[1.0, 1.0], [-1.0, -1.0]], dtype=np.float32) * 0.001
    fr = encode_vsa(9, 'constellation', cloud, ideal=ideal,
                    scalars=(244140.625, 12.5, 0.25, float('nan'), 21.5),
                    measurements={'mean_dbm': -25.25, 'duty': 1.0, 'samples': 262144,
                                  'rms_v': 0.001})
    assert fr[:4] == MAGIC_VSA
    out = decode_vsa(fr)
    assert out['version'] == 9 and out['kind'] == 'constellation'
    assert np.array_equal(out['data'], cloud)
    assert np.array_equal(out['ideal'], ideal)
    assert out['symbol_rate_hz'] == pytest.approx(244140.625)
    assert out['cfo_hz'] == pytest.approx(12.5)
    assert out['timing_samples'] == pytest.approx(0.25)
    assert out['snr_db'] == pytest.approx(21.5)
    assert out['evm_percent'] != out['evm_percent']               # NaN: Phase 2 fills it
    assert out['measurements']['mean_dbm'] == pytest.approx(-25.25)
    assert out['measurements']['duty'] == 1.0
    assert out['measurements']['samples'] == pytest.approx(262144.0)
    # Every key is present, so a decoder can index the block by name without guessing.
    assert list(out['measurements']) == list(VSA_MEASURE_KEYS)
    missing = [k for k, v in out['measurements'].items() if v != v]
    assert 'peak_dbm' in missing and 'evm_percent' in missing


@pytest.mark.parametrize('kind,shape', [('power', (5, 2)), ('ccdf', (7, 2)),
                                       ('spectrogram', (4, 8))])
def test_vsa_kinds_and_shapes(kind, shape):
    data = np.arange(np.prod(shape), dtype=np.float32).reshape(shape)
    out = decode_vsa(encode_vsa(1, kind, data))
    assert out['kind'] == kind
    assert out['data'].shape == shape
    assert out['ideal'].shape == (0, 2)
    assert out['measurements']['mean_dbm'] != out['measurements']['mean_dbm']   # all NaN


def test_vsa_refuses_a_truncated_or_foreign_frame():
    fr = encode_vsa(1, 'ccdf', np.zeros((3, 2), np.float32), measurements={'duty': 0.5})
    assert decode_vsa(fr[:-1]) is None
    assert decode_vsa(fr[:20]) is None
    assert decode_vsa(b'XXXX' + fr[4:]) is None
    assert decode_vsa(encode_powr(1, np.zeros(4, np.float32), 0.0)) is None


def test_vsa_data_must_be_two_dimensional():
    with pytest.raises(ValueError, match='2-D'):
        encode_vsa(1, 'power', np.zeros(4, dtype=np.float32))
    with pytest.raises(ValueError, match='2-D'):
        encode_vsa(1, 'power', np.zeros((2, 2), np.float32), ideal=np.zeros(2))
