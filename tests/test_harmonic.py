"""Harmonic measurement failure semantics.

Reported defects, both bench-visible:
  - the fundamental fetch used to succeed-or-not silently: when it failed, the FIRST
    successful harmonic lent its amplitude as the dBc base, so H2 was compared against H2
    and every later dBc number was nonsense;
  - a failing `configure_swp` inside the per-harmonic sequence returned no frames and no
    error forever — the measurement looked hung instead of escalating.
"""
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.hardware.errors import DeviceError
from web_sa.measurements.harmonic import HarmonicSession

F0 = 100e6


def make_dev(sweeps):
    """`sweeps`: one entry per _measure_step fetch — an (freqs, pows) pair or None."""
    return SimpleNamespace(
        state=SimpleNamespace(
            caps=SimpleNamespace(freq_min_hz=9e3, freq_max_hz=3e9),
            center_hz=F0, span_hz=10e6,
        ),
        configure_swp=lambda: (True, 'ok'),
        fetch_sweep=lambda: sweeps.pop(0) if sweeps else None,
    )


def measure_session(dev, count=3):
    session = HarmonicSession(dev)
    session.f0 = F0
    session.count = count
    session._scan_phase = False
    return session


def test_base_amplitude_only_comes_from_the_fundamental():
    # A failed step is six None fetches (the retry loop); the next sweeps are the H2/H3 data.
    dev = make_dev([None, None, None, None, None, None,        # fundamental step fails
                    (np.array([200e6]), np.array([-10.0])),    # H2 succeeds
                    (np.array([300e6]), np.array([-20.0]))])   # H3 succeeds
    session = measure_session(dev)

    _, msgs = session.step()          # n=1: failed
    assert session._base is None      # H2 must never lend the base

    session.step()                    # n=2
    session.step()                    # n=3
    rows = session._seq
    assert rows[0]['amp'] is None and rows[0]['dbc'] is None
    assert rows[1]['amp'] == pytest.approx(-10.0)
    assert rows[1]['dbc'] is None     # no base: say so instead of inventing 0 dBc
    assert rows[2]['amp'] == pytest.approx(-20.0)
    assert rows[2]['dbc'] is None


def test_fundamental_reference_is_used_for_dbc():
    dev = make_dev([(np.array([100e6]), np.array([-30.0])),
                    (np.array([200e6]), np.array([-50.0]))])
    session = measure_session(dev)

    session.step()
    session.step()

    rows = session._seq
    assert rows[0]['dbc'] == pytest.approx(0.0)
    assert rows[1]['dbc'] == pytest.approx(-20.0)


def test_repeated_configure_failures_escalate():
    dev = make_dev([])
    dev.configure_swp = lambda: (False, 'SWP_Configuration status=-9')
    session = measure_session(dev)

    assert session.step() == ([], [])          # one failure: retry
    assert session.step() == ([], [])          # two failures: retry
    with pytest.raises(DeviceError, match='harmonic configure failed'):
        session.step()                          # three: escalate to the link recovery
