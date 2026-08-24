"""DeviceState 序列化 + build_status(WS STATUS 载荷) 单测(无设备, stub)"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from web_sa.hardware.device import DeviceState
from web_sa.config import DeviceCapabilities
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


def test_device_state_serializable():
    """状态字段全部可 JSON 序列化(无 NaN/对象)"""
    import json
    s = make_state(gnss={'lock': True, 'sats': 8},
                   actual={'center': 1e9}, harm_results=[{'n': 1, 'amp': -20.5}])
    json.dumps(build_status(StubDevice(s)))
    assert s.gnss['sats'] == 8
    assert s.harm_results[0]['amp'] == -20.5
