"""framer 编解码 + dtype guard 单测"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np

from web_sa.measurements.framer import decode_powr, encode_freq, encode_powr, parse_header


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
