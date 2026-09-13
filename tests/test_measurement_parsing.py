"""Harmonic and phase-noise result assembly (report finding P2-7).

Both sessions build their result payloads from device data; that assembly had no direct
tests because it lived inside the DLL-calling step. The harmonic sequence is driven here
through a stub device (configure_swp/fetch_sweep), and the phase-noise payload is built by
the extracted pure helper - no hardware, no DLL.
"""
from __future__ import annotations

import threading
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.config import DeviceCapabilities
from web_sa.measurements.harmonic import HarmonicSession
from web_sa.measurements.results import pnm_payload


class StubDevice:
    """Enough device for a harmonic sequence: state, lock and a synthetic sweep."""

    def __init__(self, peak_at=lambda center: center, peak_dbm=lambda center: -60.0):
        caps = DeviceCapabilities.from_model(67)
        self.state = SimpleNamespace(
            caps=caps, center_hz=1e9, span_hz=10e6, rbw_hz=1e5, vbw_hz=1e5, window=1,
            harm_results=[], pnm_last=None,
        )
        self._hw = threading.RLock()
        self.session = None
        self._peak_at = peak_at
        self._peak_dbm = peak_dbm
        self.sweeps = 0

    def configure_swp(self):
        return True, 'ok'

    def fetch_sweep(self):
        self.sweeps += 1
        s = self.state
        points = 101
        freq = np.linspace(s.center_hz - s.span_hz / 2, s.center_hz + s.span_hz / 2, points)
        pows = np.full(points, -90.0)
        # One peak, 30 dB above the noise, at the requested centre (or wherever the stub says)
        idx = int(np.argmin(np.abs(freq - self._peak_at(s.center_hz))))
        pows[idx] = self._peak_dbm(s.center_hz)
        return freq, pows


def run_harmonic(dev, count=3):
    session = HarmonicSession(dev)
    session.set_params(f0=1e9, count=count, span=5e6)
    session._scan_phase = False        # go straight to the per-harmonic measurement
    messages = []
    for _ in range(20):                # bounded: the sequence ends by itself
        _frames, msgs = session.step()
        messages.extend(msgs)
        if messages and messages[-1].get('cmd') == 'HARM':
            break
    return messages


def test_harmonic_sequence_reports_each_order_with_dbc_relative_to_the_first():
    dev = StubDevice()
    messages = run_harmonic(dev, count=3)
    assert messages, 'the sequence must publish a HARM result'
    result = messages[-1]
    assert result['cmd'] == 'HARM' and result['f0'] == 1e9
    assert [item['n'] for item in result['list']] == [1, 2, 3]
    # each harmonic is measured at its own frequency (n * f0), clamped into the device range
    assert result['list'][1]['f'] == pytest.approx(2e9, rel=1e-3)
    # the first harmonic is the reference: its dBc is 0 by definition
    assert result['list'][0]['dbc'] == pytest.approx(0.0, abs=1e-6)
    assert all(item['dbc'] == pytest.approx(item['amp'] - result['list'][0]['amp'], abs=0.01)
               for item in result['list'])
    assert dev.state.harm_results == result['list']


def test_harmonic_amplitude_follows_the_measured_peak():
    # A peak 20 dB weaker from the 2nd order on: the reported dBc must follow it.
    dev = StubDevice(peak_dbm=lambda center: -60.0 if center < 1.5e9 else -80.0)
    result = run_harmonic(dev, count=3)[-1]
    assert result['list'][0]['amp'] > result['list'][1]['amp']


def test_harmonic_stops_at_the_device_frequency_limit():
    dev = StubDevice()
    session = HarmonicSession(dev)
    dev.state.caps.freq_max_hz = 2.2e9          # f0=1e9 -> only H1 and H2 fit
    session.set_params(f0=1e9, count=5, span=5e6)
    session._scan_phase = False
    messages = []
    for _ in range(20):
        _frames, msgs = session.step()
        messages.extend(msgs)
        if messages and messages[-1].get('cmd') == 'HARM':
            break
    assert [item['n'] for item in messages[-1]['list']] == [1, 2]


def test_harmonic_set_params_clamps_to_the_device():
    dev = StubDevice()
    session = HarmonicSession(dev)
    session.set_params(f0=1e12, count=99, span=1e12)
    assert session.f0 == dev.state.caps.freq_max_hz
    assert session.count == 10
    assert session.span == 100e6
    session.set_params(f0=0.0, count=0, span=0.0)
    assert session.f0 == dev.state.caps.freq_min_hz
    assert session.count == 1
    assert session.span == 1.0


def test_phase_noise_payload_shape():
    freq = np.array([100.0, 1000.0, 10000.0])
    pn = np.array([-95.5, -105.25, -120.0])
    payload = pnm_payload(carrier_freq=1e9, carrier_power=-20.5, offset=freq, pn=pn,
                          ref=-21.0, traceavg=4, done=True, progress=100)
    assert payload['cmd'] == 'PNM'
    assert payload['carrier_freq'] == 1e9
    assert payload['carrier_power'] == -20.5
    assert payload['offset'] == [100.0, 1000.0, 10000.0]
    assert payload['pn'] == [-95.5, -105.25, -120.0]
    assert payload['ref'] == -21.0
    assert payload['traceavg'] == 4.0
    assert payload['done'] is True and payload['progress'] == 100


def test_phase_noise_payload_progress_is_optional():
    payload = pnm_payload(carrier_freq=1e9, carrier_power=-20.0, offset=[100.0], pn=[-95.0],
                          ref=-21.0, traceavg=4, done=False, progress=25)
    assert payload['done'] is False and payload['progress'] == 25
