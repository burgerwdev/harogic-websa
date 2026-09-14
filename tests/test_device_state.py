"""DeviceState 序列化 + build_status(WS STATUS 载荷) 单测(无设备, stub)"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceState, HarogicDevice
from web_sa.web.http_api import build_status


class StubDevice:
    """Minimal stub for build_status: .state + .preset_defaults + the device interface."""
    def __init__(self, state: DeviceState):
        self.state = state
        self.preset_defaults = {'center': 1e9, 'span': 100e6}
        self.session = None

    def auto_reference_view(self) -> dict:
        return {'last_peak': None, 'last_noise_floor': None, 'target': None,
                'result': 'idle', 'pending': None, 'adjusting': False}

    def session_health(self) -> dict:
        return {}


def make_state(**kw) -> DeviceState:
    s = DeviceState()
    for k, v in kw.items():
        setattr(s, k, v)
    return s


def test_build_status_shape():
    dev = StubDevice(make_state(connected=True, center_hz=1e9, span_hz=100e6,
                                points_req=1001, window=1, spur_mode='standard',
                                caps=DeviceCapabilities.from_model(67)))
    st = build_status(dev)
    assert st['cmd'] == 'STATUS'
    assert st['connected'] is True
    assert st['center'] == 1e9 and st['span'] == 100e6
    assert st['points'] == 1001
    assert st['window'] == 1
    assert st['spur'] == 'standard'
    # req/actual 结构
    assert st['req']['center'] == 1e9
    assert 'actual' in st
    # caps 型号推导
    assert st['caps']['model'] == 67
    assert st['caps']['name'] == 'SAN-90'
    assert st['caps']['fmax'] == 9e9


def test_build_status_defaults():
    dev = StubDevice(make_state())   # 全默认
    st = build_status(dev)
    assert st['connected'] is False
    assert st['caps'] == {'model': 0, 'name': '', 'fmin': 0, 'fmax': 0}
    assert st['ref_clock'] == 'internal'
    assert st['has_docxo'] is False
    assert st['ref_mode'] == 'manual'
    assert st['rta_defaults']['span'] == 50.78125e6


def test_build_status_keeps_swp_and_rta_settings_independent():
    state = make_state(
        mode='rta',
        center_hz=2e9,
        span_hz=200e6,
        rbw_mode='manual',
        rbw_hz=200e3,
        vbw_mode='manual',
        vbw_hz=50e3,
        rta_center_hz=1e9,
        rta_span_hz=50.78125e6,
        rta_rbw_mode='auto',
        rta_vbw_mode='equal',
        rta_actual={
            'center': 1e9,
            'span': 50.78125e6,
            'ref': 0.0,
            'rbw': 30153.0,
            'vbw': 30153.0,
            'points': 1001,
        },
    )
    status = build_status(StubDevice(state))
    assert status['span'] == 50.78125e6
    assert status['rbw_mode'] == 'auto'
    assert status['vbw_mode'] == 'equal'
    assert status['rbw'] == 30153.0
    assert status['req']['swp']['rbw'] == 200e3
    assert status['req']['swp']['vbw'] == 50e3
    assert status['req']['rta']['span'] == 50.78125e6


def test_device_state_serializable():
    """状态字段全部可 JSON 序列化，SDK 非有限浮点值清洗为 null。"""
    import json
    s = make_state(gnss={'lock': True, 'sats': 8, 'latitude': float('nan')},
                   actual={'center': 1e9, 'rbw': float('inf')},
                   harm_results=[{'n': 1, 'amp': -20.5}])
    status = build_status(StubDevice(s))
    json.dumps(status, allow_nan=False)
    assert status['gnss']['latitude'] is None
    assert status['actual']['rbw'] is None
    assert s.gnss['sats'] == 8
    assert s.harm_results[0]['amp'] == -20.5


def test_auto_scale_places_the_reference_once():
    """Auto Scale is an action, not a mode: one decision, and no target for the other mode."""
    dev = HarogicDevice()
    dev.state.mode = 'std'
    dev.state.ref_level = 0.0
    dev.observe_reference_peak('std', -27.0)
    assert dev.auto_ref.pending is None          # observing never moves the level by itself
    # No noise-floor estimate -> keep the peak 10 dB below the top, quantised to 5 dB.
    assert dev.auto_scale('std') == ('applied', -15.0)
    assert dev.auto_ref.pending == ('std', -15.0)
    assert dev.state.rta_ref_level == 0.0


def test_auto_scale_is_a_no_op_when_the_placement_is_good():
    dev = HarogicDevice()
    dev.state.ref_level = -50.0
    dev.state.ref_range_db = 100.0
    dev.observe_reference_peak('std', -125.0, -145.0)
    assert dev.auto_scale('std') == ('ok', None)
    assert dev.auto_ref.pending is None


def test_auto_scale_target_survives_another_mode():
    dev = HarogicDevice()
    dev.state.mode = 'std'
    dev.state.ref_level = -20
    dev.observe_reference_peak('std', 0)
    assert dev.auto_scale('std') == ('applied', 10.0)
    assert dev.auto_ref.pending == ('std', 10.0)

    dev.state.mode = 'rta'
    assert not dev.apply_pending_auto_reference()
    assert dev.auto_ref.pending == ('std', 10.0)


def test_auto_reference_anchors_on_the_noise_floor_not_the_peak():
    """Auto Ref places the NOISE FLOOR just above the bottom of the display window.

    That is what a spectrum analyser does (it maximises the visible dynamic range above the
    noise), and it is why the peak must not drive the decision: with peak+5 the noise floor
    ended up mid-screen for weak signals. Window = ref_range_db (10 div x 10 dB = 100 dB).
    """
    dev = HarogicDevice()
    dev.state.ref_level = -20.0
    dev.state.ref_range_db = 100.0
    dev.observe_reference_peak('std', -80.0, -95.0)
    # -95 + 100 - 8 = -3 -> 0 after quantisation (the peak would have said -70).
    assert dev.auto_scale('std') == ('applied', 0.0)
    # A tall window with a high noise floor pushes Ref up instead.
    dev.auto_ref.pending = None
    dev.observe_reference_peak('std', -50.0, -70.0)
    assert dev.auto_scale('std') == ('applied', 25.0)


def test_auto_reference_learns_the_if_overflow_floor():
    """-12 means the IF saturates: raise Ref and never propose that level again.

    Without the learned floor the peak-based rule kept lowering Ref again and the two
    mechanisms fought, oscillating 5-10 dB (measured on hardware).
    """
    dev = HarogicDevice()
    dev.state.mode = 'std'
    dev.state.ref_level = -50.0
    dev.state.status_warning = -12
    assert dev.nudge_reference_out_of_overflow()
    assert dev.auto_ref.pending == ('std', -45.0)
    assert dev.auto_ref.tracker('std')['floor'] == -45.0
    # Neither the overflow escape nor a later Auto Scale may drive Ref below the learned floor.
    dev.auto_ref.pending = None
    dev.state.status_warning = 0
    dev.state.ref_level = -30.0
    dev.observe_reference_peak('std', -125.0, -145.0)
    assert dev.auto_scale('std') == ('applied', -45.0)


def test_auto_reference_keeps_a_shorter_window_within_range():
    """A 5 dB/div window (50 dB tall) must not push Ref off the top of its range."""
    dev = HarogicDevice()
    dev.state.ref_level = -20.0
    dev.state.ref_range_db = 50.0
    dev.observe_reference_peak('std', -50.0, -70.0)
    # -70 + 50 - 8 = -28 -> -25 after quantisation, versus 25 for the 100 dB window.
    assert dev.auto_scale('std') == ('applied', -25.0)


def test_rta_defaults_can_be_reset_without_touching_swp():
    dev = HarogicDevice()
    dev.state.center_hz = 2e9
    dev.state.rbw_hz = 200e3
    dev.state.rta_center_hz = 3e9
    dev.state.rta_span_hz = 12_695_312.5
    dev.state.rta_vbw_mode = 'manual'
    dev.reset_rta_state()
    assert dev.state.rta_center_hz == 1e9
    assert dev.state.rta_span_hz == 50.78125e6
    assert dev.state.rta_rbw_mode == 'auto'
    assert dev.state.rta_vbw_mode == 'equal'
    assert dev.state.rta_sweep_time_mode == 2
    assert dev.state.center_hz == 2e9
    assert dev.state.rbw_hz == 200e3


def test_full_span_preset_uses_capability_midpoint():
    dev = HarogicDevice()
    dev.state.caps = DeviceCapabilities.from_model(67)
    dev.preset_defaults = {
        'center': 4.510004e9,
        'span': 9.019992e9,
        'ref': 0.0,
        'rbw': 250e3,
        'vbw': 300e3,
        'points': 4000,
        'atten': -1,
        'window': 1,
        'rbw_mode': 'auto',
        'vbw_mode': 'bypass',
        'spur': 'standard',
        'preamp': 0,
        'ifgain': 2,
        'gain_strategy': 0,
        'sweep_time_mode': 0,
        'sweep_time': 0.0,
    }
    dev.preset_state()
    assert dev.state.center_hz == (9e3 + 9e9) / 2
    assert dev.state.span_hz == 9e9 - 9e3


def test_auto_reference_holds_when_the_placement_is_already_good():
    """The window criterion: a well-placed trace must not be re-adjusted.

    Acts only when the noise floor would leave its band above the bottom edge or the peak
    loses headroom, so a wobbling estimate (or a small RBW/points change that moves the
    noise floor by a dB or two) cannot trigger a reconfigure.
    """
    dev = HarogicDevice()
    dev.state.ref_range_db = 100.0
    dev.state.ref_level = -50.0
    for _ in range(3):
        # noise -145 sits 5 dB above the bottom edge (-150); peak keeps 75 dB of headroom.
        dev.observe_reference_peak('std', -125.0, -145.0)
    assert dev.auto_ref.pending is None
    assert dev.auto_scale('std') == ('ok', None)

    # A placement that has left its band is fitted on request: here Ref is far too high for
    # the 40 dB window, so the floor sits on the bottom edge with no room above it.
    dev.state.ref_level = -20.0
    dev.state.ref_range_db = 40.0
    dev.observe_reference_peak('std', -40.0, -60.0)      # -> target -25
    assert dev.auto_scale('std') == ('applied', -25.0)
    assert dev.auto_ref.pending == ('std', -25.0)


def test_out_of_window_trace_is_fitted_without_being_asked():
    """Measured regression: Ref 0 dBm, 80 dB window, everything at -108 dBm.

    The old loop needed a peak more than 15 dB above the noise floor and therefore refused to
    move, leaving the display empty while the signal was away. The safety ranger fixes a trace
    that has left the window regardless of its peak-to-noise ratio.
    """
    dev = HarogicDevice()
    dev.state.ref_level = 0.0
    dev.state.ref_range_db = 80.0
    dev.observe_reference_peak('std', -98.0, -108.0)
    assert dev.auto_ref.pending == ('std', -35.0)


def test_manual_attenuation_does_not_disable_the_overflow_escape():
    """Selecting a manual attenuator used to switch overload protection off silently."""
    dev = HarogicDevice()
    dev.state.atten = 10
    dev.state.ref_level = -40.0
    dev.state.status_warning = -12
    assert dev.nudge_reference_out_of_overflow()
    assert dev.auto_ref.pending == ('std', -35.0)
