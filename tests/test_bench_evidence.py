"""A benchmark cannot pass using stale frames or missing/nonfinite measurements."""
import asyncio
import copy
import json
from types import SimpleNamespace

import aiohttp
import pytest

from tools.bench import bench
from tools.bench.bench import compare, measure_switch


def test_mode_switch_waits_for_confirmed_target_before_counting_its_frame():
    messages = [
        (aiohttp.WSMsgType.BINARY, b'RTAF' + bytes(28)),
        (aiohttp.WSMsgType.TEXT, json.dumps({'cmd': 'STATUS', 'mode': 'rta'})),
        (aiohttp.WSMsgType.BINARY, b'RTAF' + bytes(28)),
        (aiohttp.WSMsgType.TEXT, json.dumps({'cmd': 'STATUS', 'mode': 'sdr'})),
        (aiohttp.WSMsgType.BINARY, b'RTA2' + bytes(28)),
    ]
    class Socket:
        reads = 0
        async def send_json(self, _payload):
            pass
        async def receive(self):
            kind, data = messages[self.reads]
            self.reads += 1
            return SimpleNamespace(type=kind, data=data)
    socket = Socket()
    assert asyncio.run(measure_switch(socket, {'cmd': 'SET_MODE', 'mode': 'sdr'})) >= 0
    assert socket.reads == 5


@pytest.mark.parametrize('bad', [None, float('nan'), float('inf')])
@pytest.mark.parametrize('metric', ['switch_to_sdr_ms', 'cpu_s', 'fps'])
def test_baseline_comparison_cannot_pass_missing_or_nonfinite_metrics(bad, metric):
    baseline = {'switch_to_sdr_ms': 50., 'swp': {'fps': 10., 'cpu_s': 1., 'clients': 1}}
    current = copy.deepcopy(baseline)
    if metric == 'switch_to_sdr_ms':
        current[metric] = bad
    else:
        current['swp'][metric] = bad
    assert compare(current, baseline)


def test_sdr_display_fps_has_the_same_regression_floor_as_other_modes():
    baseline = {'sdr': {'fps': 20., 'cpu_s': 1., 'audio': {'fps': 50.}}}
    current = copy.deepcopy(baseline)
    current['sdr']['fps'] = 5.
    assert any('sdr fps' in problem for problem in compare(current, baseline))


def test_cpu_meter_uses_the_requested_listener_not_the_first_native_process(monkeypatch):
    monkeypatch.setattr(bench.subprocess, 'run', lambda *_a, **_kw: SimpleNamespace(stdout='101\n202\n'))
    monkeypatch.setattr(bench, '_owns_listener', lambda pid, port: pid == 202 and port in (8080, 8099), raising=False)
    assert bench.worker_pid() == 202
    assert bench.worker_pid('http://127.0.0.1:8099') == 202
    assert bench.worker_pid('http://127.0.0.1:8098') is None
    assert bench.worker_pid('http://192.0.2.1:8099') is None, 'remote measurements cannot use a local worker CPU'
