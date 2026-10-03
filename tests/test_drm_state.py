"""DRM backend wiring: the 'drm' demod publishes metadata and AUDF audio to STATUS.

Runs against the fake device, so no vendor SDK, PulseAudio or Dream binary is needed:
the real decoder path lives in `measurements/sdr.py` and is covered by the
`tests/test_drm_dream*.py` integration tests. This test pins the contract the UI
and /api/state rely on.
"""
from __future__ import annotations

import pytest

from web_sa.hardware.fake_device import FakeDevice, install_fake_sessions
from web_sa.measurements import _SESSIONS, make_session
from web_sa.measurements.fake import DRM_METADATA, FakeSdrSession
from web_sa.web.http_api import build_status


@pytest.fixture
def device():
    dev = FakeDevice()
    ok, err = dev.open()
    assert ok, err
    return dev


@pytest.fixture
def fake_sessions():
    original = dict(_SESSIONS)
    install_fake_sessions()
    try:
        yield
    finally:
        _SESSIONS.clear()
        _SESSIONS.update(original)


def test_drm_mode_emits_audio_and_metadata(device, fake_sessions):
    sdr = make_session(device, 'sdr')
    assert isinstance(sdr, FakeSdrSession)
    sdr.set_demod(mode='drm')
    assert device.state.sdr_demod == 'drm'

    frames, _msgs = sdr.step()
    magics = {frame[:4] for frame in frames}
    assert b'AUDF' in magics
    # DRM is demodulated by the background Dream decoder, not by the browser IQBF path.
    assert b'IQBF' not in magics

    drm = device.state.sdr_drm
    assert drm['active'] is True
    assert drm['station'] == DRM_METADATA['station']
    assert drm['robustness'] == 'B'
    assert drm['bitrate_kbps'] == pytest.approx(20.96)
    assert drm['sync'] is True


def test_status_payload_exposes_drm_metadata(device, fake_sessions):
    sdr = make_session(device, 'sdr')
    sdr.set_demod(mode='drm')
    sdr.step()

    status = build_status(device)
    assert status['sdr']['demod'] == 'drm'
    assert status['sdr']['actual']['demod'] == 'drm'
    assert status['sdr']['drm']['station'] == 'SAN90 DRM TEST'
    assert status['sdr']['drm']['robustness'] == 'B'


def test_leaving_drm_clears_the_metadata(device, fake_sessions):
    sdr = make_session(device, 'sdr')
    sdr.set_demod(mode='drm')
    sdr.step()
    assert device.state.sdr_drm.get('active') is True

    sdr.set_demod(mode='am')
    sdr.step()
    # The fake only publishes DRM metadata while the mode is 'drm'; leaving it drops the field.
    assert device.state.sdr_drm == {}
