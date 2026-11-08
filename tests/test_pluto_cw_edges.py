"""CW ramping must attenuate samples inside both edges of every keyed mark."""
import importlib.util
from pathlib import Path

import numpy as np


def test_cw_ramps_both_edges_and_recovers_for_the_next_mark():
    path = Path(__file__).resolve().parents[1] / 'tools/pluto/pluto_cw_tx.py'
    spec = importlib.util.spec_from_file_location('cw_edges', path)
    cw = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(cw)
    # Two 20 ms marks, including one at sample zero, with a 10 ms gap.
    key = np.r_[np.ones(20), np.zeros(10), np.ones(20)].astype(np.float32)
    cycle = cw.build_cycle(key, 1000., .06, 0., 1.)
    edge = np.sin(np.pi * np.arange(5) / 10.) ** 2
    mark = np.r_[edge, np.ones(10), edge[::-1]]
    expected = np.r_[mark, np.zeros(10), mark, np.zeros(10)]
    np.testing.assert_allclose(cycle.real, expected, atol=1e-7, rtol=0)
    np.testing.assert_array_equal(cycle.imag, np.zeros(60))
