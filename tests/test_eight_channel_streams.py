"""A nonbinary output rate must retain exact source cadence and detect a real gap."""
from __future__ import annotations

import importlib.util
import json
import struct
import threading
import time
from pathlib import Path

import numpy as np
import pytest
from websockets.sync.server import serve

spec = importlib.util.spec_from_file_location('eight_streams',
    Path(__file__).resolve().parents[1] / 'tools/bench/eight_channel_streams.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


@pytest.mark.parametrize('offset', [-173.37, .13, 96.84])
@pytest.mark.parametrize('seconds', [.11, 1.01])
@pytest.mark.parametrize('expected', [-1800, 2000])
def test_rf_source_reference_measures_real_rate_despite_dc_and_noise(offset, seconds, expected):
    # This reference controls only the transmitter. The receive/decoder path
    # continues to see and validate the physical RF signal without correction.
    rf_spec = importlib.util.spec_from_file_location('eight_rf',
        Path(__file__).resolve().parents[1] / 'tools/bench/eight_channel_rf.py')
    rf = importlib.util.module_from_spec(rf_spec)
    rf_spec.loader.exec_module(rf)
    rate = 48828.125
    time_axis = np.arange(round(rate * seconds)) / rate
    noise = np.random.default_rng(27).normal(size=(2, len(time_axis)))
    samples = 10 + np.exp(2j * np.pi * (expected + offset) * time_axis) + .01 * (noise[0] + 1j * noise[1])
    assert abs(rf.carrier_frequency(samples, rate, expected) - (expected + offset)) < (.2 if seconds < 1 else .03)
    with pytest.raises(AssertionError, match='No isolated RF'):
        rf.carrier_frequency(np.zeros(len(time_axis)), rate, expected)


def test_nonbinary_iq_rate_keeps_exact_positions_and_rejects_one_sample_gap():
    send_gap, finished = threading.Event(), threading.Event()
    identity = {'source_id': 'primary', 'channel_id': '17', 'epoch': '3'}
    def packet(sequence, position):
        return struct.pack('<4sIIIddIIQd', b'IQBF', 2, sequence, 100, 62500000 / 192,
            435000000.0, 17, 3, position, 1.0) + struct.pack('<200f', *([.125] * 200))
    def handler(socket):
        socket.send(json.dumps({'type': 'stream_start', 'receiver_uid': 'eighth', 'stream': identity,
            'clock': {'input_rate_hz': 62500000, 'actual_rate_hz': 62500000 / 192}}))
        socket.send(packet(1, 12345)); socket.send(packet(2, 12345 + 100 * 192))
        assert send_gap.wait(10)
        socket.send(packet(3, 12345 + 200 * 192 + 1))
        finished.wait(10)
    with serve(handler, '127.0.0.1', 0) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        snapshot = {'service_id': 'fixture', 'capture': {'input_rate_hz': 62500000},
            'receivers': [{'receiver_uid': 'eighth', 'display_number': 8, 'stream': identity,
                'confirmed': {'purpose': 'iq', 'processors': []}}]}
        try:
            with module.StreamMonitor(f'http://127.0.0.1:{server.socket.getsockname()[1]}', snapshot) as monitor:
                deadline = time.monotonic() + 5
                while monitor.status()['8-iq']['packets'] < 2:
                    assert time.monotonic() < deadline
                    time.sleep(.01)
                assert monitor.status()['8-iq']['samples'] == 200
                send_gap.set()
                while not monitor.errors:
                    assert time.monotonic() < deadline
                    time.sleep(.01)
                with pytest.raises(AssertionError, match='8-iq'):
                    monitor.check()
        finally:
            send_gap.set(); finished.set(); server.shutdown(); thread.join(timeout=5)
