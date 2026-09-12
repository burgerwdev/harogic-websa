"""SDR DSP and lifecycle regression tests; no hardware required."""
from __future__ import annotations

import struct
import threading
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.demod.ddc import DdcChannel
from web_sa.demod.demod import AnalogDemod
from web_sa.demod.spectrum import Panadapter
from web_sa.hardware import sdk_bindings as sb
from web_sa.measurements import sdr as sdr_module
from web_sa.measurements.sdr import SdrSession, _round_decimate, encode_audio


def _rms(values: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.asarray(values, dtype=np.float64) ** 2)))


def test_round_decimate_uses_supported_power_of_two():
    assert _round_decimate(1) == 1
    assert _round_decimate(31) == 16
    assert _round_decimate(2049) == 2048
    assert _round_decimate('bad') == 16


def test_audio_frame_keeps_compatible_header():
    pcm = np.array([-1, 2, -3], dtype=np.int16)
    frame = encode_audio(7, 48000, pcm)
    assert struct.unpack_from('<4sIII', frame) == (b'AUDF', 7, 48000, 3)
    assert np.array_equal(np.frombuffer(frame, dtype=np.int16, offset=16), pcm)


def test_panadapter_crops_iq_guard_band():
    pan = Panadapter(fft_size=1024)
    iq = np.ones(1024)
    result = pan.process(iq, np.zeros_like(iq), 1e6, 100e6, 1.0, bandwidth=800e3)
    assert result is not None
    freq, power, row = result
    assert 800 <= len(freq) <= 822
    assert len(power) == len(row) == len(freq)
    assert freq[0] >= 99.6e6
    assert freq[-1] <= 100.4e6


def test_cw_zero_if_carrier_becomes_configured_sidetone():
    fs = 48000.0
    demod = AnalogDemod(fs)
    demod.configure(fs, 'cw', 500.0, pitch=700.0)
    audio, _ = demod.process(np.ones(96000), np.zeros(96000), use_agc=False)
    audio = audio[4000:]
    peak = np.fft.rfftfreq(audio.size, 1.0 / fs)[np.argmax(np.abs(np.fft.rfft(audio)))]
    assert _rms(audio) > 0.1
    assert peak == pytest.approx(700.0, abs=2.0)


def test_wfm_applies_50us_deemphasis():
    fs = 48000.0
    t = np.arange(96000) / fs

    def demodulated_rms(tone_hz: float) -> float:
        deviation = 1000.0
        phase = (deviation / tone_hz) * np.sin(2.0 * np.pi * tone_hz * t)
        z = np.exp(1j * phase)
        demod = AnalogDemod(fs)
        demod.configure(fs, 'wfm', 180000.0)
        audio, _ = demod.process(z.real, z.imag, use_agc=False)
        return _rms(audio[8000:])

    low = demodulated_rms(1000.0)
    high = demodulated_rms(10000.0)
    assert high / low == pytest.approx(0.316, rel=0.15)


def test_hard_failure_recovery_restarts_full_chain(monkeypatch):
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=SimpleNamespace(last_error=''))
    session._error_streak = 7
    session._last_recovery = 0.0
    session._recovery_attempts = 0
    called = []
    session._reconfigure_full_locked = lambda: called.append('full')
    monkeypatch.setattr(sdr_module.time, 'monotonic', lambda: 10.0)

    session._step_failed_locked('get', -1)

    assert called == ['full']
    assert session._recovery_attempts == 1


def test_demod_reconfiguration_stops_stream_first(monkeypatch):
    state = SimpleNamespace(
        sdr_demod='am', sdr_if_bw=6000.0, sdr_pitch=700.0,
        sdr_squelch=-110.0, sdr_volume=0.8, sdr_agc=True,
    )
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state)
    session._lock = threading.RLock()
    calls = []
    session._stop_trigger_locked = lambda: calls.append('stop')
    session._configure_chain_locked = lambda: calls.append('configure')
    session._start_trigger_locked = lambda: calls.append('start')
    monkeypatch.setattr(sdr_module.time, 'monotonic', lambda: 20.0)

    session.set_demod(mode='wfm', if_bw=180000.0)

    assert calls == ['stop', 'configure', 'start']
    assert session._ready_at == pytest.approx(20.15)


def test_demod_reconfiguration_rolls_back_state_on_failure():
    state = SimpleNamespace(
        sdr_demod='am', sdr_if_bw=6000.0, sdr_pitch=700.0,
        sdr_squelch=-110.0, sdr_volume=0.8, sdr_agc=True,
    )
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state)
    session._lock = threading.RLock()

    def fail():
        raise RuntimeError('configure failed')

    session._reconfigure_chain_runtime_locked = fail
    with pytest.raises(RuntimeError, match='configure failed'):
        session.set_demod(mode='wfm', if_bw=180000.0, pitch=900.0)

    assert (state.sdr_demod, state.sdr_if_bw, state.sdr_pitch) == ('am', 6000.0, 700.0)


def test_settle_window_still_drains_iqs(monkeypatch):
    state = SimpleNamespace(sdr_listen_hz=101.7e6)
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state, dev=sb.c_void_p())
    session._lock = threading.RLock()
    session._ready = True
    session._ready_at = 11.0
    session._applied_listen = state.sdr_listen_hz
    session._audio_reset_pending = True
    session._audio_seq = 0
    session._last_ok = 10.0
    session._last_recovery = 0.0
    session._last_status = 0
    session._transient_streak = 0
    session._timeout_streak = 0
    session._recovery_attempts = 0
    session._packets_ok = 0
    session._packets_err = 0
    called = []

    def fake_get(_dev, _stream):
        called.append(True)
        return 0

    monkeypatch.setattr(sdr_module.time, 'monotonic', lambda: 10.0)
    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fake_get)

    frames, _ = session.step()

    assert called == [True]
    assert len(frames) == 1
    assert struct.unpack_from('<4sIII', frames[0]) == (b'AUDF', 0, 48000, 0)


def test_ddc_rejects_oversized_native_output(monkeypatch):
    channel = DdcChannel(sb.c_void_p())
    channel._ready = True
    channel.sample_points = 16
    channel.out_points = 4
    channel.delay = 0
    channel.fs_in = 1e6

    def fake_execute(_dsp, _ins, outs_ptr):
        outs_ptr.contents.IQS_StreamInfo.PacketSamples = 100
        return 0

    monkeypatch.setattr(sb.dll, 'DSP_DDC_Execute', fake_execute)
    iq = np.zeros(32, dtype=np.int16)
    with pytest.raises(RuntimeError, match='invalid points'):
        channel.process(iq, 16)
