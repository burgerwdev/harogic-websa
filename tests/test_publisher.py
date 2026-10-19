"""Acquisition policy: the session owns the watchdog and the pacing (report finding E-3)."""
from __future__ import annotations

import asyncio
import json
import threading
from types import SimpleNamespace

from web_sa.hardware.device import DeviceState, HarogicDevice, _estimated_sweep_ms
from web_sa.measurements.base import StdSession
from web_sa.measurements.rta import RtaSession
from web_sa.measurements.sdr import SdrSession
from web_sa.web.app_keys import COMMAND_LOCK, WS_CLIENTS
from web_sa.web.publisher import _acquisition_step, _acquisition_timeout


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
    assert sdr.pacing(0.001, False) == 0.002  # no good packet: back off
    sdr._last_step_ok = True
    assert sdr.pacing(0.001, False) == 0.0   # coalesced IQ, no frame yet: keep draining
    sdr._last_step_ok = False
    assert sdr.pacing(0.001, True) == 0.002  # a reset marker is not a good IQ packet


def test_only_the_swept_session_dedupes_the_frequency_axis():
    assert StdSession(StubDevice(DeviceState())).dedupe_freq is True
    assert RtaSession(StubDevice(DeviceState(mode='rta'))).dedupe_freq is False
    assert SdrSession(StubDevice(DeviceState(mode='sdr'))).dedupe_freq is False


def test_high_rate_publisher_batches_complete_iq_steps_not_sdk_ddc_blocks():
    class Device:
        state = DeviceState(mode='sdr', sdr_actual={'iq_rate': 15_625_000})
        session = SimpleNamespace(name='sdr', _last_step_ok=True)
        def __init__(self):
            self.steps = self.nudges = 0
        def nudge_reference_out_of_overflow(self):
            self.nudges += 1
        def apply_pending_auto_reference(self):
            return False
        def step(self):
            self.steps += 1
            return [f'iq-{self.steps}'.encode()], [{'step': self.steps}]

    dev = Device()
    assert _acquisition_step(dev) == ([b'iq-1', b'iq-2'], [{'step': 1}, {'step': 2}])
    assert (dev.steps, dev.nudges) == (2, 2)
    dev.state.sdr_actual['iq_rate'] = 7_812_500
    assert _acquisition_step(dev) == ([b'iq-3'], [{'step': 3}])
    dev.state.sdr_actual['iq_rate'] = 15_625_000
    dev.session._last_step_ok = False
    assert _acquisition_step(dev) == ([b'iq-4'], [{'step': 4}])
    dev.step = lambda: None
    assert _acquisition_step(dev) is None  # older session contracts allow no result


def test_sweep_estimate_converts_sdk_seconds_to_frame_milliseconds():
    assert _estimated_sweep_ms(SimpleNamespace(EstimateMinSweepTime=0.0125)) == 12.5
    assert _estimated_sweep_ms(SimpleNamespace(EstimateMinSweepTime=0.0)) == 0.0


def test_publisher_sweep_ema_does_not_overwrite_device_estimate():
    dev = HarogicDevice.__new__(HarogicDevice)
    dev.state = DeviceState(sweep_ms=12.5)
    dev._sweep_ema = None
    dev.measure_sweep(0.020)
    dev.measure_sweep(0.030)
    assert dev.state.sweep_ms == 12.5
    assert dev.state.sweep_ms_meas == 22.0


class Client:
    """Minimal ClientStream stand-in: records what the publisher fanned out."""

    dropped_frames = dropped_control = dropped_audio = dropped_iq = 0
    #: The publisher asks whether a client wants raw IQ before the session encodes it.
    accepts_iq = False
    accepts_audio = True
    accepts_aux = False

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
