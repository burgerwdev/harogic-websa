"""VSA session: lifecycle, mode-private state, capture/stream paths and recovery.

No hardware and no vendor library: the device is the fake one used by the CI UI smoke, and
the IQS stream is a scripted stub, so the capture / progress / re-arm logic is driven
deterministically. measurements/vsa.py imports the vendor layer lazily (through iqs.py),
which is what lets these run in CI.
"""
from __future__ import annotations

from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.hardware.errors import DeviceError
from web_sa.hardware.fake_device import FakeDevice, install_fake_sessions
from web_sa.measurements import _SESSIONS, make_session
from web_sa.measurements.iqs import Fetch
from web_sa.measurements.vsa import VsaSession
from web_sa.web.commands import COMMANDS, CommandError, validate

PACKET = 4096


class FakeIqs:
    """A scripted stand-in for iqs.IqsStream (same surface the session uses)."""

    def __init__(self, *, packet_samples=PACKET, statuses=(), recover=(), settling_until=0.0):
        self.packet_samples = packet_samples
        self.scale_to_v = 2e-5
        self.packets_ok = 0
        self.packets_err = 0
        self.transient_streak = 0
        self.timeout_streak = 0
        self.last_status = 0
        self.last_ok = 0.0
        self.last_recovery = 0.0
        self.ready_at = settling_until
        self.calls: list = []
        self.statuses = list(statuses)
        self.recover_flags = list(recover)
        self.stream = object()

    # -- configuration --
    def profile(self, **kw):
        # Mirrors the real profile struct: the session sets CenterFreq_Hz and the device
        # echoes the centre it applied.
        self.calls.append(('profile', kw))
        return SimpleNamespace(NativeIQSampleRate_SPS=62.5e6,
                               CenterFreq_Hz=float(kw.get('center_hz', 1e9)))

    def mode_reset(self):
        self.calls.append('mode_reset')

    def configure(self, p, *, mode_reset=True):
        self.calls.append(('configure', mode_reset))
        # The device echoes the centre it actually applied (as the real one does).
        return SimpleNamespace(fs=3.90625e6, bandwidth=3.125e6,
                               center_hz=float(getattr(p, 'CenterFreq_Hz', 100e6)),
                               decimate=16, packet_samples=self.packet_samples,
                               packet_bytes=self.packet_samples * 4, packet_count=4)

    def start(self):
        self.calls.append('start')

    def stop(self, *, required=True):
        self.calls.append(('stop', required))

    # -- stream --
    def settling(self, now=None):
        return self.ready_at > (0.0 if now is None else now)

    def arm_settle(self, now=None, delay=None):
        self.ready_at = 0.0

    def fetch(self, now=None):
        status = self.statuses.pop(0) if self.statuses else 0
        if status != 0:
            self.packets_err += 1
            self.last_status = status
            return Fetch(status, transient=True,
                         recover=self.recover_flags.pop(0) if self.recover_flags else False,
                         warn=status in (-8, -9, -10, -12))
        self.packets_ok += 1
        t = np.arange(self.packet_samples)
        raw = (20000 * np.sin(2 * np.pi * t / 64.0)).astype(np.int16)
        raw = np.repeat(raw, 2)[:self.packet_samples * 2]
        return Fetch(0, raw=raw, samples=self.packet_samples, scale_to_v=self.scale_to_v)

    def note_recovery(self, now=None):
        self.calls.append('note_recovery')
        self.last_recovery = 0.0
        self.transient_streak = 0


@pytest.fixture
def device():
    dev = FakeDevice()
    ok, err = dev.open()
    assert ok, err
    return dev


def _session(device, iqs=None) -> VsaSession:
    return VsaSession(device, iqs=iqs or FakeIqs())


# ---------------- lifecycle and mode-private state ----------------

def test_enter_snapshots_the_swp_config_and_exit_restores_it(device):
    s = device.state
    s.center_hz, s.span_hz, s.points_req = 101.7e6, 2e6, 800
    session = _session(device)
    session.enter()
    # A VSA capture does not touch the SWP window...
    assert (s.center_hz, s.span_hz) == (101.7e6, 2e6)
    session.exit()
    # ...and leaving restores exactly what was there before.
    assert (s.center_hz, s.span_hz, s.points_req) == (101.7e6, 2e6, 800)
    assert session.snapshot is None


def test_vsa_geometry_is_mode_private(device):
    session = _session(device)
    session.enter()
    session.set_params(center=433.92e6, decimate=64, view='stream', depth=1 << 20)
    s = device.state
    assert (s.vsa_center_hz, s.vsa_decimate) == (433.92e6, 64)
    assert (s.vsa_view, s.vsa_depth) == ('stream', 1 << 20)
    session.exit()
    session.enter()
    # Re-entering keeps the user's VSA settings (mode-private, MODE_STATE_FLOW).
    assert (s.vsa_center_hz, s.vsa_decimate, s.vsa_view) == (433.92e6, 64, 'stream')
    assert s.center_hz != 433.92e6              # the SWP window was not stolen


def test_exit_stops_the_stream_and_resets_the_device_mode(device):
    iqs = FakeIqs()
    session = _session(device, iqs)
    session.enter()
    session.exit()
    assert ('stop', False) in iqs.calls
    assert 'mode_reset' in iqs.calls            # so the restored SWP config takes effect


def test_request_stop_makes_step_a_no_op(device):
    session = _session(device)
    session.enter()
    session.request_stop()
    assert session.step() == ([], [])


# ---------------- capture path ----------------

def test_capture_accumulates_its_frame_then_publishes_once(device):
    iqs = FakeIqs()
    session = _session(device, iqs)
    device.state.vsa_depth = PACKET * 2
    session.enter()
    session.iqs.ready_at = 0.0                 # settle window already over
    frames, _ = session.step()
    assert frames == [] and device.state.vsa_progress == pytest.approx(0.5)
    assert device.state.vsa_busy is True
    frames, _ = session.step()
    assert len(frames) == 1 and frames[0][:4] == b'RTAF'
    assert device.state.vsa_last['points'] > 0        # a spectrum was measured from the frame
    # A published capture arms the next frame (progress restarts) instead of pretending to
    # be a live stream.
    assert device.state.vsa_busy is True
    assert device.state.vsa_progress == 0.0
    assert session.health()['frames'] == 1


def test_capture_reports_the_configured_geometry(device):
    iqs = FakeIqs()
    session = _session(device, iqs)
    device.state.vsa_depth = PACKET
    session.enter()
    actual = device.state.vsa_actual
    assert actual['iq_rate'] == 3.90625e6
    assert actual['depth'] == PACKET
    assert actual['packets'] == 4
    assert actual['start'] < actual['iq_center'] < actual['stop']
    configured = [c for c in iqs.calls if isinstance(c, tuple) and c[0] == 'configure']
    assert configured and configured[0][1] is True    # configure(..., mode_reset=True)


def test_stream_view_does_not_report_a_capture(device):
    iqs = FakeIqs()
    session = _session(device, iqs)
    device.state.vsa_view = 'stream'
    session.enter()
    assert device.state.vsa_busy is False
    assert device.state.vsa_actual['depth'] == 0


def test_stream_publishes_at_the_display_rate(device, monkeypatch):
    iqs = FakeIqs()
    session = _session(device, iqs)
    device.state.vsa_view = 'stream'
    session.enter()
    session.iqs.ready_at = 0.0
    clock = [100.0]
    monkeypatch.setattr('web_sa.measurements.vsa.time.monotonic', lambda: clock[0])
    assert len(session.step()[0]) == 1          # first packet paints
    clock[0] += 0.005
    assert session.step()[0] == []              # inside the 20 Hz interval
    clock[0] += 0.100
    assert len(session.step()[0]) == 1


# ---------------- recovery ----------------

def test_a_wedged_stream_is_re_armed(device):
    iqs = FakeIqs(statuses=[-9], recover=[True])
    session = _session(device, iqs)
    session.enter()
    session.iqs.ready_at = 0.0
    before = len([c for c in iqs.calls if isinstance(c, tuple) and c[0] == 'configure'])
    assert session.step() == ([], [])
    after = len([c for c in iqs.calls if isinstance(c, tuple) and c[0] == 'configure'])
    assert after == before + 1                  # re-armed in place
    assert 'note_recovery' in iqs.calls
    assert session.health()['recovery_attempts'] == 1


def test_repeated_wedges_escalate_to_a_worker_restart(device):
    session = _session(device)
    session.enter()
    for _ in range(VsaSession.RECOVERY_LIMIT):
        session._rearm('test')
    with pytest.raises(DeviceError, match='unrecoverable'):
        session._rearm('test')


def test_health_reports_the_stream_counters(device):
    iqs = FakeIqs()
    session = _session(device, iqs)
    session.enter()
    session.iqs.ready_at = 0.0
    device.state.vsa_depth = PACKET
    session.step()
    health = session.health()
    assert health['ok'] >= 1 and health['err'] == 0
    assert health['phase'] in ('settle', 'capture', 'stream')
    assert set(health) >= {'ok', 'err', 'last_status', 'transient_streak', 'progress',
                           'frames', 'recovery_attempts'}


# ---------------- command layer ----------------

def _dev_for_validate(device, mode='vsa'):
    device.state.mode = mode
    device.session = None
    return device


def test_set_mode_accepts_vsa():
    choices = next(p for p in COMMANDS['SET_MODE'].params if p.name == 'mode').choices
    assert 'vsa' in choices


def test_set_vsa_spec_is_registered_with_bounded_parameters(device):
    spec = COMMANDS['SET_VSA']
    by_name = {p.name: p for p in spec.params}
    assert set(by_name) == {'center', 'decimate', 'view', 'depth', 'measure', 'modulation',
                            'symbol_rate', 'rolloff', 'phase_rot'}
    assert by_name['view'].choices == ('capture', 'stream')
    assert by_name['measure'].choices == ('spectrum',)     # only what the session produces
    assert by_name['depth'].minimum == 1024
    with pytest.raises(CommandError, match='at least one parameter'):
        validate(_dev_for_validate(device), 'SET_VSA', {})
    validate(_dev_for_validate(device), 'SET_VSA', {'depth': 1 << 20})     # accepted


def test_vsa_mode_refuses_the_commands_it_owns_itself(device):
    with pytest.raises(CommandError) as exc:
        validate(_dev_for_validate(device, 'vsa'), 'SET_FREQ', {'center': 100e6})
    assert exc.value.code == 'vsa_unsupported'
    # The same command stays available in the swept mode.
    validate(_dev_for_validate(device, 'std'), 'SET_FREQ', {'center': 100e6})


# ---------------- fake backend: modes do not disturb each other ----------------

def test_fake_backend_round_trips_through_vsa(device, monkeypatch):
    original = dict(_SESSIONS)
    install_fake_sessions()
    try:
        device.state.center_hz = 101.7e6        # before entering: this is the snapshot
        rta = make_session(device, 'rta')
        device.state.rta_center_hz = 88.5e6
        rta.step()

        vsa = make_session(device, 'vsa')
        assert vsa.name == 'vsa' and device.state.mode == 'vsa'
        vsa.set_params(center=433.92e6)
        for _ in range(4):
            vsa.step()
        assert device.state.vsa_last                            # produced a result

        back = make_session(device, 'rta')
        assert back.name == 'rta' and device.state.mode == 'rta'
        assert device.state.rta_center_hz == 88.5e6             # RTA state untouched
        assert device.state.center_hz == 101.7e6                # SWP state untouched
        assert device.state.vsa_center_hz == 433.92e6           # VSA kept its own
    finally:
        _SESSIONS.clear()
        _SESSIONS.update(original)
