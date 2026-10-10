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
    SDR_MAX_CAPTURE_DECIMATE,
    SdrSession,
    _round_decimate,
    display_pan_points,
    sdr_capture_geometry,
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


def test_a_widened_capture_keeps_the_display_window_on_the_request():
    """The operator reported: the SDR spectrum and the audio became choppy below ~100 kHz capture.

    The publisher loop is paced by the packet. The IQS Adaptive packet has a fixed size of about
    16240 samples. Therefore a narrow capture makes the packet period long (133 ms at 97.7 kHz,
    532 ms at 24.4 kHz). That period is too long for the panadapter and for the audio target of
    the browser. Thus the device capture is floored, and the narrower request of the operator
    becomes the DISPLAY window. The payload must keep the two windows apart, because the front end
    draws the display window and clips to it.
    """
    w = sdr_spectrum_windows(101.7e6, 101.7e6, 97.66e3, 390.625e3)
    assert w['stop'] - w['start'] == pytest.approx(97.66e3)          # the requested span
    assert w['capture_stop'] - w['capture_start'] == pytest.approx(390.625e3)
    assert w['capture_start'] == pytest.approx(101.7e6 - 390.625e3 / 2.0)
    assert w['capture_stop'] == pytest.approx(101.7e6 + 390.625e3 / 2.0)
    # The capture reaches past the display on both sides (the centres are equal here).
    assert w['capture_start'] < w['start'] and w['capture_stop'] > w['stop']


def test_capture_geometry_floors_the_device_and_shows_the_request():
    native = 62.5e6
    for requested in (512, 1024, 2048):
        capture, display, capture_bw = sdr_capture_geometry(requested, native)
        assert capture == SDR_MAX_CAPTURE_DECIMATE == 128
        assert display == pytest.approx(native * 0.8 / requested)     # the span of the operator
        assert capture_bw == pytest.approx(native * 0.8 / capture)
        assert capture_bw > display
    # The device accepts this request. It goes through without a change (display == capture).
    for requested in (1, 16, 32, 128):
        capture, display, capture_bw = sdr_capture_geometry(requested, native)
        assert capture == requested and display == capture_bw
    # The device reports no native rate. The session invents no floor and no window.
    assert sdr_capture_geometry(512, 0.0) == (SDR_MAX_CAPTURE_DECIMATE, 0.0, 0.0)


def test_the_capture_floor_keeps_the_packet_period_inside_the_panadapter_cadence():
    """The floor has one purpose: the step rate must not fall below the rate of the consumers.

    PacketDataSize stays at about 64960 bytes at every decimate. That value is 16240 complex-int16
    samples for one packet (measured on the SAN-90). Therefore the period is
    ``packet_samples / IQSampleRate``, and the session emits the panadapter frame (20 fps maximum)
    and the channelized baseband block one time for each period. Every decimate that the UI offers
    must operate at the floor or higher. The three reported decimates are not sufficient.
    """
    native = 62.5e6
    packet_samples = 16240
    for requested in (1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048):
        capture, _display, _capture_bw = sdr_capture_geometry(requested, native)
        period = packet_samples / (native / capture)
        assert period <= 1.0 / 20.0, (requested, period)
    # The symptom, in numbers (nominal, from the configured packet). If the request itself is the
    # device geometry, the periods are 133/266/532 ms at 512/1024/2048. The panadapter accepts
    # 20 fps, and the browser ring keeps a 0.3 s target. On the bench the baseband blocks were
    # 129/253/483 ms, and the panadapter ran at 10.3/5.4/3.0 frames/s, with 9 ring underruns in
    # 8 s at 2048.
    for requested, naive_ms in ((512, 133.0), (1024, 266.1), (2048, 532.2)):
        naive = packet_samples / (native / requested)
        assert naive == pytest.approx(naive_ms / 1e3, rel=0.01)
        assert naive > 1.0 / 20.0


def test_display_pan_points_follow_the_display_window_not_the_capture():
    """The FFT grid covers the whole capture. Only the part of the grid in the display is real."""
    # 16240 bins over a 488 kSPS capture. A 24.4 kHz display holds 5 % of them.
    assert display_pan_points(16240, 24.41e3, 488.28e3, 2048) == 812
    # The NumPy fallback has only PAN_FFT bins over the same capture.
    assert display_pan_points(2048, 24.41e3, 488.28e3, 2048) == 102
    # The value is wider than the point budget. The cap of the panadapter applies.
    assert display_pan_points(16240, 97.66e3, 488.28e3, 2048) == 2048
    # Degenerate input still gives a drawable frame (not more than cap, not less than 2).
    assert display_pan_points(0, 0.0, 0.0, 2048) == 2048
    assert display_pan_points(16240, 1.0, 62.5e6, 2048) == 2


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


def test_cw_carrier_at_the_pitch_becomes_the_sidetone():
    """The operator's tuning: the dial sits `pitch` below the carrier, so it arrives at +pitch.

    The demodulator selects a narrow band there (the sidetone *is* that pitch). A zero-beat carrier
    - the previous design's requirement, and where the receiver's DC cancellation lives - is not
    demodulated at all.
    """
    fs = 48000.0
    t = np.arange(96000) / fs
    carrier = 0.5 * np.exp(2j * np.pi * 700.0 * t)
    demod = AnalogDemod(fs)
    demod.configure(fs, 'cw', 500.0, pitch=700.0)
    audio, _ = demod.process(carrier.real, carrier.imag, use_agc=False)
    audio = audio[4000:]
    peak = np.fft.rfftfreq(audio.size, 1.0 / fs)[np.argmax(np.abs(np.fft.rfft(audio)))]
    assert _rms(audio) > 0.1
    assert peak == pytest.approx(700.0, abs=2.0)

    zero_beat = AnalogDemod(fs)
    zero_beat.configure(fs, 'cw', 500.0, pitch=700.0)
    dc_audio, _ = zero_beat.process(np.ones(96000), np.zeros(96000), use_agc=False)
    assert _rms(dc_audio[4000:]) < 0.01 * _rms(audio), 'a zero-beat carrier must be rejected'


def test_cw_demod_band_covers_the_decoder_pitch_search():
    """The demodulator must not limit the frequency tolerance of the decoder.

    The CW engine searches the CW audio band (250 Hz to 1500 Hz). See `CW_SEARCH_HZ` in
    frontend/src/dsp/ggmorseEngine.ts. The demodulator selects the band `pitch +/- if_bw/2` above
    zero IF. At the CW default of 3 kHz, this band is -800 Hz to +2200 Hz. Thus it contains each
    sidetone that the decoder can read, and the demodulator is not the limit. This result shows that
    the decoder caused the reported fault, and not the filter. In the report, the audio was
    satisfactory and the decode failed.

    A narrow filter is a limit. The second part of the test shows this condition. The operator
    selects the filter width. The panel selects 3 kHz for CW automatically.
    """
    fs = 48000.0
    t = np.arange(96000) / fs
    decoder_band = (250.0, 1500.0)

    def demodulated(tone_hz: float, if_bw: float) -> float:
        with_agc = AnalogDemod(fs)
        with_agc.configure(fs, 'cw', if_bw, pitch=700.0)
        carrier = 0.5 * np.exp(2j * np.pi * tone_hz * t)
        audio, _ = with_agc.process(carrier.real, carrier.imag, use_agc=False)
        return _rms(audio[4000:])

    # Each sidetone that the engine can decode is present after the demodulator, at the CW default
    # filter.
    for tone in (300.0, 700.0, 1100.0, 1450.0):
        level = demodulated(tone, 3000.0)
        assert level > 0.1, f'{tone} Hz sidetone lost at the 3 kHz CW filter (rms {level})'

    # The band contains the full search range of the decoder. Therefore the sidetone is present.
    pitch, if_bw = 700.0, 3000.0
    assert pitch - if_bw / 2.0 <= decoder_band[0]
    assert pitch + if_bw / 2.0 >= decoder_band[1]

    # A narrow filter removes this tolerance. The operator selects the filter width. This result is
    # not a fault.
    assert demodulated(1200.0, 500.0) < 0.1 * demodulated(700.0, 500.0), (
        'a 500 Hz CW filter should reject a sidetone 500 Hz outside it')



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


def test_bandwidth_switch_rolls_back_and_reconfigures_when_it_fails():
    """A failed SET_SDR must not report a capture that the device did not take.

    Before, one failed bandwidth switch left two problems. The panel kept the new bandwidth,
    because STATUS reads `state.sdr_decimate` and `set_params` wrote it before the configuration.
    The stream was down as well: `_configure` clears `_ready` first, and `step()` returns
    immediately while `_ready` is false. Nothing else would configure the session again.
    """
    state = SimpleNamespace(sdr_center_hz=100e6, sdr_decimate=32, sdr_listen_hz=100e6)
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state)
    attempts: list[int] = []

    def configure():
        attempts.append(len(attempts) + 1)
        if len(attempts) == 1:
            raise RuntimeError('IQS_Configuration status=-11')

    session._configure = configure
    with pytest.raises(RuntimeError, match='status=-11'):
        session.set_params(decimate=1024)

    # The failed attempt, then the restore attempt that puts the old geometry back.
    assert len(attempts) == 2
    assert state.sdr_decimate == 32
    assert state.sdr_center_hz == 100e6


def test_bandwidth_switch_reports_the_original_error_when_the_restore_also_fails():
    state = SimpleNamespace(sdr_center_hz=100e6, sdr_decimate=32, sdr_listen_hz=100e6)
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state)

    def fail():
        raise RuntimeError('device bus is down')

    session._configure = fail
    with pytest.raises(RuntimeError, match='device bus is down'):
        session.set_params(center=101e6)

    # The state still describes the last capture that worked, not the one that failed.
    assert (state.sdr_center_hz, state.sdr_decimate) == (100e6, 32)


def test_deferred_deemph_is_published_while_the_browser_owns_audio():
    """`sdr.actual.deemph_us` must describe the chain that is running, not the idle fallback.

    With the browser DSP owning the audio a de-emphasis change is deferred (`_chain_stale`), and
    `sdr_actual` used to keep the Python chain's last resolved figure - so the e2e contract
    ("de-emphasis is applied") passed against the fake backend (which publishes the requested
    value) and failed on real hardware.
    """
    state = SimpleNamespace(
        sdr_demod='nfm', sdr_if_bw=6000.0, sdr_pitch=700.0, sdr_deemph_us=-1.0,
        sdr_squelch=-110.0, sdr_volume=0.8, sdr_agc=True,
        sdr_actual={'deemph_us': 0.0},
    )
    session = SdrSession.__new__(SdrSession)
    session.dev = SimpleNamespace(state=state, audio_clients=0)
    session._lock = threading.RLock()
    session._chain_stale = False

    session.set_demod(deemph_us=75)

    assert session._chain_stale is True          # the Python chain rebuild waits for its turn
    assert state.sdr_actual['deemph_us'] == 75.0  # STATUS reports the tau the listener hears


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


def test_ddc_rate_stays_close_to_the_if_bandwidth():
    """The DDC's output rate is the *browser* demodulator's per-sample cost.

    A rate 2.5x the IF bandwidth put WFM (180 kHz) at a 460 kHz baseband, where the demodulator
    measured 97.5% of real time - it could not keep up, so the playback stretched instead of playing.
    1.6x keeps the band (0.5x) plus its filter transition inside the requested rate.
    """
    session = SdrSession.__new__(SdrSession)
    session._fs_in = 62.5e6 / 8                      # 7.8 MSps capture
    session.dev = SimpleNamespace(state=SimpleNamespace(sdr_if_bw=180_000.0))
    if_bw, decimate = session._chain_params()
    assert if_bw == pytest.approx(180_000.0)
    fs_out = session._fs_in / decimate
    assert fs_out < 2.0 * if_bw, fs_out          # 1.6x, not the old 2.5x
    assert fs_out > if_bw, fs_out                # and never below the band itself
    # A narrow mode still gets the 48 kHz floor (the demodulator's own rate).
    session.dev = SimpleNamespace(state=SimpleNamespace(sdr_if_bw=6_000.0))
    _, decimate = session._chain_params()
    assert session._fs_in / decimate == pytest.approx(48_000.0, rel=0.05)


def test_demod_channel_is_clamped_to_the_display_window_not_the_capture():
    """A wider capture must not widen the channel of the demodulator.

    The browser runs a 257-tap complex band filter at the DDC output rate. Thus the clamp follows
    the *display* window (what the operator requested to receive). It does not follow the wider
    capture that keeps the IQS packet period short.
    """
    session = SdrSession.__new__(SdrSession)
    session._fs_in = 62.5e6 / 128                      # 488 kSPS: the floored capture
    session._display_bw = 24_000.0                     # a 24 kHz request inside it
    session.dev = SimpleNamespace(state=SimpleNamespace(sdr_if_bw=180_000.0))
    if_bw, decimate = session._chain_params()
    assert if_bw == pytest.approx(24_000.0 * 0.4)     # clamped by the window, not by the capture
    assert session._fs_in / decimate == pytest.approx(48_000.0, rel=0.05)


def test_baseband_rate_is_measured_not_assumed():
    """The browser maps baseband samples onto the sound card's clock.

    A vendor's nominal DDC rate is not the rate the device delivers, so a session that declared the
    nominal figure made the browser produce slightly the wrong number of samples per second - the
    ring drained and the audio puffed about once a second. The rate is measured over a window
    instead, and the nominal value is only used until the first window closes.
    """
    session = SdrSession.__new__(SdrSession)
    session._ddc = SimpleNamespace(fs_out=48_828.125)
    session._bb_rate = 0.0
    session._bb_window_samples = 0
    session._bb_window_start = 0.0
    # Before any measurement the nominal rate is reported (the first second or so of a stream).
    assert session._baseband_rate() == pytest.approx(48_828.125)
    assert session._audio_rate() == pytest.approx(session.AUDIO_RATE)

    # 406 samples every 8.3 ms: a stream that really runs at 48.9 kHz.
    now = 100.0
    session._measure_baseband_rate(0, now)                 # a step that produced nothing
    for _ in range(250):                                    # >2 s of packets
        session._measure_baseband_rate(406, now)
        now += 406 / 48_900.0
    measured = session._measured_bb_rate()
    assert measured == pytest.approx(48_900.0, rel=0.002), measured
    assert session._baseband_rate() == pytest.approx(measured)
    # The Python demodulator's audio carries the same correction (it resamples from the nominal
    # rate, so its true output rate is scaled by the same factor).
    assert session._audio_rate() == pytest.approx(48_000.0 * measured / 48_828.125, rel=1e-6)
    # And a later window moves the estimate only slowly (the pitch must not wobble).
    # A later window moves the estimate only slowly: the declared rate is what the browser turns
    # into a resampling ratio, so a wobble would be a pitch wobble.
    before = session._baseband_rate()
    for _ in range(250):
        session._measure_baseband_rate(406, now)
        now += 406 / 48_900.0
    assert abs(session._baseband_rate() - before) < 150.0


def test_python_audio_path_only_runs_while_someone_listens():
    """The Python demodulator is the fallback/reference, not a second audio path.

    With the browser DSP owning playback nothing subscribes to AUDF, and the demodulator (the most
    expensive stage of the step) must not run for sockets that would discard its output.
    """
    session = SdrSession.__new__(SdrSession)
    assert session._audio_subscribed(SimpleNamespace(audio_clients=1)) is True
    assert session._audio_subscribed(SimpleNamespace(audio_clients=0)) is False
    # A device that does not report the count keeps the previous behaviour (the path runs).
    assert session._audio_subscribed(SimpleNamespace()) is True

    # The pause/resume decision (and its one-time log) is the branch `step()` acts on, and it must
    # survive a session built without `__init__` (a test stub, or an object being reused).
    listening = SimpleNamespace(audio_clients=1)
    silent = SimpleNamespace(audio_clients=0)
    assert session._audio_paused_state(listening) is False
    assert session._audio_paused_state(silent) is True
    assert session._audio_paused_state(silent) is True          # no repeated transition
    assert session._audio_paused_state(listening) is False


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
