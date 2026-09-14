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
from web_sa.measurements.framer import encode_audio
from web_sa.measurements.sdr import (
    SdrSession,
    _round_decimate,
    sdr_spectrum_windows,
)


def _rms(values: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.asarray(values, dtype=np.float64) ** 2)))


def test_round_decimate_uses_supported_power_of_two():
    assert _round_decimate(1) == 1
    assert _round_decimate(31) == 16
    assert _round_decimate(2049) == 2048
    assert _round_decimate('bad') == 16


def test_spectrum_windows_separate_display_from_capture():
    # The device captured 200 kHz above the requested centre: the display window must stay
    # on the request (so the user's centre is the canvas centre) and the capture window must
    # report where the hardware really is.
    w = sdr_spectrum_windows(101.7e6, 101.9e6, 3.125e6)
    assert w['center'] == 101.7e6
    assert w['start'] == pytest.approx(101.7e6 - 1.5625e6)
    assert w['stop'] == pytest.approx(101.7e6 + 1.5625e6)
    assert w['capture_center'] == 101.9e6
    assert w['capture_start'] == pytest.approx(101.9e6 - 1.5625e6)
    assert w['capture_stop'] == pytest.approx(101.9e6 + 1.5625e6)
    # Both windows are the same width; only the centre moved.
    assert (w['stop'] - w['start']) == pytest.approx(w['capture_stop'] - w['capture_start'])


def test_spectrum_windows_match_when_there_is_no_offset():
    w = sdr_spectrum_windows(101.7e6, 101.7e6, 3.125e6)
    assert w['start'] == w['capture_start']
    assert w['stop'] == w['capture_stop']


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
    session.dev = SimpleNamespace(state=state, dev=sb.c_void_p(),
                                  note_link_status=lambda *a, **k: False)
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


def test_sdk_call_retries_transient_bus_warnings(monkeypatch):
    """-10/-11 are documented 're-call Configuration' warnings, not hard failures."""
    monkeypatch.setattr(sdr_module.time, 'sleep', lambda _s: None)
    session = SdrSession.__new__(SdrSession)
    statuses = iter([-11, -10, -11, 0])
    calls = []

    def fn():
        calls.append(1)
        return next(statuses)

    assert session._sdk_call(fn, 'X') == 0
    assert len(calls) == 4


def test_sdk_call_raises_after_retries_exhausted(monkeypatch):
    monkeypatch.setattr(sdr_module.time, 'sleep', lambda _s: None)
    session = SdrSession.__new__(SdrSession)
    with pytest.raises(RuntimeError, match='mode reset status=-11'):
        session._sdk_call(lambda: -11, 'mode reset')


def test_sdk_call_fails_fast_on_hard_error():
    session = SdrSession.__new__(SdrSession)
    calls = []

    def fn():
        calls.append(1)
        return -9

    with pytest.raises(RuntimeError, match='status=-9'):
        session._sdk_call(fn, 'X')
    assert len(calls) == 1


# --- AGC: no clipping burst on (re)configure, and a hard peak ceiling -----------------
# Measured on hardware before the fix: switching the demod on a real FM station produced
# nine consecutive full-scale (1.0000) WFM/NFM audio frames, and 18/5 clipped frames per
# two seconds of steady WFM/AM audio, because the AGC started at unity gain and ramped
# toward the target at only 20% per block while the FM discriminator output is in Hz.


def _agc_block(rms: float, n: int = 2048, seed: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    x = rng.standard_normal(n)
    return (x / _rms(x) * rms).astype(np.float32)


def test_agc_first_block_is_not_clipped_at_unity_gain():
    from web_sa.demod.filters import Agc
    agc = Agc(target=0.2)
    # FM discriminator output scale: several thousand, i.e. ~4 orders of magnitude off.
    y = agc.process(_agc_block(5000.0))
    assert np.max(np.abs(y)) <= 1.0, 'first block must not clip'
    assert 0.1 < _rms(y) < 0.4, 'first block should land near the target, not stay loud'


def test_agc_never_exceeds_the_peak_ceiling():
    from web_sa.demod.filters import Agc
    agc = Agc(target=0.2, ceiling=0.95)
    agc.process(_agc_block(0.2))                 # settle somewhere sensible
    for i, rms in enumerate((0.2, 3.0, 0.5, 12.0, 0.2)):
        y = agc.process(_agc_block(rms, seed=i + 1))
        assert np.max(np.abs(y)) <= 0.9500001, f'block {i} peak exceeded the ceiling'


def test_agc_hold_keeps_gain_but_still_bounds_the_peak():
    from web_sa.demod.filters import Agc
    agc = Agc(target=0.2)
    agc.process(_agc_block(0.2))
    gain_before = agc.gain
    y = agc.process(_agc_block(900.0, seed=7), hold=True)
    assert agc.gain == gain_before, 'hold must freeze the gain'
    assert np.max(np.abs(y)) <= 0.9500001


def test_agc_silence_does_not_raise_the_gain():
    from web_sa.demod.filters import Agc
    agc = Agc(target=0.2)
    agc.process(_agc_block(0.2))
    gain_before = agc.gain
    agc.process(np.zeros(512, dtype=np.float32))
    assert agc.gain == gain_before


def test_analog_demod_reconfigure_does_not_burst():
    """End-to-end: a reconfigure (new Agc at unity gain) must not emit a clipped block."""
    demod = AnalogDemod()
    n = 24000
    ph = np.cumsum(np.full(n, 2.0 * np.pi * 2000.0 / 48225.0))   # +/-2 kHz deviation
    z = np.exp(1j * (ph + 0.3 * np.sin(np.linspace(0, 60, n))))
    demod.configure(48225.0, 'wfm', 180000.0)
    audio, _ = demod.process(z.real, z.imag, use_agc=True)
    assert audio.size
    assert np.max(np.abs(audio)) <= 0.9500001
    demod.configure(48225.0, 'nfm', 12000.0)              # fresh Agc at unity gain
    audio2, _ = demod.process(z.real, z.imag, use_agc=True)
    assert audio2.size
    assert np.max(np.abs(audio2)) <= 0.9500001, 'reconfigure must not produce a burst'
