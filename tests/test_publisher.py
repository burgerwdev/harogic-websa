"""Acquisition policy: the session owns the watchdog and the pacing (report finding E-3)."""
from __future__ import annotations

import asyncio
import json
import threading

from web_sa.hardware.device import DeviceState
from web_sa.measurements.base import StdSession
from web_sa.measurements.rta import RtaSession
from web_sa.measurements.sdr import SdrSession
from web_sa.web.app_keys import COMMAND_LOCK, WS_CLIENTS
from web_sa.web.publisher import _acquisition_timeout


class StubDevice:
    """Just enough device for a session: state, the hardware lock and no DLL calls."""

    def __init__(self, state):
        self.state = state
        self._hw = threading.RLock()
        self.dsp = None          # DdcChannel only stores the handle at construction

    def configure_swp(self):        # pragma: no cover - not called by these tests
        raise AssertionError('reconfigure must not be called')


def test_rta_and_sdr_have_a_short_native_call_watchdog():
    assert RtaSession(StubDevice(DeviceState(mode='rta'))).acquisition_timeout() == 5.0
    assert SdrSession(StubDevice(DeviceState(mode='sdr'))).acquisition_timeout() == 5.0


def test_manual_sweep_timeout_scales_with_requested_time():
    state = DeviceState(mode='std', sweep_time_mode=7, sweep_time=60.0)
    assert StdSession(StubDevice(state)).acquisition_timeout() == 95.0


def test_sweep_watchdog_is_bounded():
    state = DeviceState(mode='std', actual={'est_min': 1000.0})
    assert StdSession(StubDevice(state)).acquisition_timeout() == 180.0


def test_publisher_uses_the_session_policy():
    """The publisher delegates; a session-less device falls back to the swept watchdog."""
    dev = StubDevice(DeviceState(mode='rta'))
    assert _acquisition_timeout(dev) == 10.0                 # sweep-scaled fallback (10 s floor)
    dev.session = RtaSession(dev)
    assert _acquisition_timeout(dev) == 5.0


def test_pacing_is_owned_by_the_session():
    state = DeviceState(mode='std')
    std = StdSession(StubDevice(state))
    assert std.pacing(0.001, True) == 0.003                  # 250 fps cap
    assert std.pacing(0.010, True) == 0.002                  # never a busy loop
    sdr = SdrSession(StubDevice(DeviceState(mode='sdr')))
    assert sdr.pacing(0.001, True) == 0.0                    # IQS paces itself
    assert sdr.pacing(0.001, False) == 0.002                 # transient error: back off


def test_only_the_swept_session_dedupes_the_frequency_axis():
    assert StdSession(StubDevice(DeviceState())).dedupe_freq is True
    assert RtaSession(StubDevice(DeviceState(mode='rta'))).dedupe_freq is False
    assert SdrSession(StubDevice(DeviceState(mode='sdr'))).dedupe_freq is False


class Client:
    """Minimal ClientStream stand-in: records what the publisher fanned out."""

    dropped_frames = dropped_control = dropped_audio = 0

    def __init__(self):
        self.texts: list[str] = []
        self.frames: list[bytes] = []

    def publish_bytes(self, frame):
        self.frames.append(frame)

    def publish_text(self, text, coalesce=False):
        self.texts.append(text)


class DisconnectedDevice:
    """Device whose USB link was declared lost; stepping it must never happen."""

    def __init__(self):
        self.state = DeviceState()
        self.state.connected = False
        self.preset_defaults = None
        self.session = None
        self.stepped = False

    def auto_reference_view(self):
        return {'last_peak': None, 'last_noise_floor': None, 'target': None,
                'result': 'idle', 'seq': 0, 'pending': None, 'adjusting': False}

    def session_health(self):
        return {}

    def nudge_reference_out_of_overflow(self):
        raise AssertionError('no hardware call while the link is down')

    def apply_pending_auto_reference(self):
        raise AssertionError('no hardware call while the link is down')

    def step(self):
        self.stepped = True
        return [], []

    def measure_sweep(self, dt):
        pass


def test_publisher_stops_acquisition_and_reports_a_disconnected_link():
    """A dead link must not be stepped, and STATUS must say so (the reported frozen spectrum)."""
    from web_sa.web import publisher as pub

    dev = DisconnectedDevice()
    client = Client()
    app = {WS_CLIENTS: {client}, COMMAND_LOCK: asyncio.Lock()}

    async def run():
        task = asyncio.create_task(pub.publisher(app, dev))
        await asyncio.sleep(0.15)
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)

    asyncio.run(run())

    assert dev.stepped is False
    assert dev.state.last_error == ''
    statuses = [json.loads(text) for text in client.texts if '"STATUS"' in text]
    assert statuses, 'the disconnect must be pushed to the client'
    assert statuses[-1]['connected'] is False
