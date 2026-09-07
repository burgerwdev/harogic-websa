"""DeviceState 序列化 + build_status(WS STATUS 载荷) 单测(无设备, stub)"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceState, HarogicDevice
from web_sa.web.http_api import build_status


class StubDevice:
    """build_status 所需的极简 stub: .state + .preset_defaults"""
    def __init__(self, state: DeviceState):
        self.state = state
        self.preset_defaults = {'center': 1e9, 'span': 100e6}


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


def test_auto_reference_uses_stable_peak_and_mode_private_target():
    dev = HarogicDevice()
    dev.state.mode = 'std'
    dev.state.ref_mode = 'auto'
    dev.state.ref_level = 0.0
    dev.state.atten = -1
    dev.observe_reference_peak('std', -27.0)
    dev._auto_ref['std']['candidate_since'] -= 2.0
    dev.observe_reference_peak('std', -27.0)
    assert dev._pending_auto_ref == ('std', -20.0)
    assert dev.state.rta_ref_level == 0.0


def test_auto_reference_is_suspended_by_manual_attenuation():
    dev = HarogicDevice()
    dev.state.ref_mode = 'auto'
    dev.state.atten = 10
    for _ in range(20):
        dev.observe_reference_peak('std', -27.0)
    assert dev._pending_auto_ref is None


def test_auto_reference_raise_is_stable_and_pending_survives_other_mode():
    dev = HarogicDevice()
    dev.state.mode = 'std'
    dev.state.ref_mode = 'auto'
    dev.state.ref_level = -20
    dev.observe_reference_peak('std', 0)
    assert dev._pending_auto_ref is None
    dev._auto_ref['std']['candidate_since'] -= 0.2
    dev.observe_reference_peak('std', 0)
    assert dev._pending_auto_ref == ('std', 5.0)

    dev.state.mode = 'rta'
    assert not dev.apply_pending_auto_reference()
    assert dev._pending_auto_ref == ('std', 5.0)


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
