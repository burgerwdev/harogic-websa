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


def test_scan_monitor_includes_broadcast_protocol_evidence_without_changing_receiver_purpose():
    receivers = [dict(receiver_uid='fm', confirmed=dict(purpose='listen', processors=[dict(mode='wfm')])),
                 dict(receiver_uid='am', confirmed=dict(purpose='listen', processors=[dict(mode='am')]))]
    snapshot = dict(capture=dict(input_rate_hz=62500000), receivers=receivers)
    monitor = module.StreamMonitor('http://127.0.0.1', snapshot, all_iq=True, include_broadcast_decode=True)
    assert monitor.expected_connections == 5
    assert monitor.decode_uids == {'fm'}
    assert all(r['confirmed']['purpose'] == 'listen' for r in receivers)


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


def test_all_iq_monitor_detects_a_gap_in_the_first_of_sixteen_receivers():
    send_gap, finished = threading.Event(), threading.Event()
    receivers = [{'receiver_uid': f'uid-{slot}', 'display_number': slot,
        'stream': {'source_id': 'primary', 'channel_id': str(slot), 'epoch': '3'},
        'confirmed': {'purpose': 'iq', 'processors': []}} for slot in range(1, 17)]

    def handler(socket):
        slot = int(socket.request.path.rsplit('-', 1)[1])
        receiver = receivers[slot - 1]
        socket.send(json.dumps({'type': 'stream_start', 'receiver_uid': receiver['receiver_uid'],
            'stream': receiver['stream']}))
        def packet(sequence, position):
            return struct.pack('<4sIIIddIIQd', b'IQBF', 2, sequence, 100, 62500000 / 192,
                435000000.0, slot, 3, position, 1.0) + struct.pack('<200f', *([.125] * 200))
        socket.send(packet(1, 12345)); socket.send(packet(2, 12345 + 19200))
        assert send_gap.wait(10)
        if slot == 1:
            socket.send(packet(3, 12345 + 38401))
        finished.wait(10)

    with serve(handler, '127.0.0.1', 0) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        snapshot = {'service_id': 'fixture', 'capture': {'input_rate_hz': 62500000}, 'receivers': receivers}
        try:
            with module.StreamMonitor(f'http://127.0.0.1:{server.socket.getsockname()[1]}', snapshot,
                    all_iq=True) as monitor:
                deadline = time.monotonic() + 5
                while len(monitor.status()) < 16 or any(row['packets'] < 2 for row in monitor.status().values()):
                    assert time.monotonic() < deadline
                    time.sleep(.01)
                assert {row['receiver_uid'] for row in monitor.status().values()} == {r['receiver_uid'] for r in receivers}
                send_gap.set()
                while not monitor.errors:
                    assert time.monotonic() < deadline
                    time.sleep(.01)
                with pytest.raises(AssertionError, match='1-iq'):
                    monitor.check()
        finally:
            send_gap.set(); finished.set(); server.shutdown(); thread.join(timeout=5)


def test_sixteen_rf_source_has_independently_demodulatable_tones(tmp_path):
    from types import SimpleNamespace
    rf_spec = importlib.util.spec_from_file_location('rf_source',
        Path(__file__).resolve().parents[1] / 'tools/bench/eight_channel_rf.py')
    rf = importlib.util.module_from_spec(rf_spec); rf_spec.loader.exec_module(rf)
    radio = rf.BenchRadio(tmp_path)
    radio.rate = 521000
    radio.sdr = SimpleNamespace(tx_lo=rf.CENTER)
    radio.settings = lambda: {'sample_rate': radio.rate}
    transmitted = []
    radio.push_cyclic = transmitted.append
    frequencies = [rf.CENTER + (slot - 7.5) * 20000 for slot in range(16)]
    tones = [400 + slot * 100 for slot in range(16)]
    modes = rf.MODES * 2
    radio.narrow(frequencies=frequencies, tones=tones, modes=modes)
    signal = transmitted[0]
    assert np.isfinite(signal).all() and max(np.max(np.abs(signal.real)), np.max(np.abs(signal.imag))) < 32767
    # Independent ideal channel filters and envelope/phase/product detectors.
    # Verify what would reach the ADC; do not reuse the waveform generator.
    time_axis = np.arange(len(signal)) / radio.rate
    bins = np.fft.fftfreq(len(signal), 1 / radio.rate)
    for frequency, tone, mode in zip(frequencies, tones, modes, strict=True):
        spectrum = np.fft.fft(signal * np.exp(-2j * np.pi * (frequency - rf.CENTER) * time_axis))
        spectrum[np.abs(bins) > 8000] = 0
        baseband = np.fft.ifft(spectrum)
        if mode == 'am':
            audio = np.abs(baseband)
        elif mode == 'nfm':
            audio = np.angle(baseband[1:] * np.conj(baseband[:-1]))
        else:
            audio = baseband.real
        measured = np.abs(np.fft.rfft((audio - audio.mean()) * np.hanning(len(audio))))
        peak = int(np.argmax(measured))
        assert abs(peak * radio.rate / len(audio) - tone) < 1
        assert measured[peak] > 100 * np.median(measured[10:400])
