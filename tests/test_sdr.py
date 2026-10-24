"""Independent SDR reference DSP and SDK boundary regression tests."""
from __future__ import annotations

import struct

import numpy as np
import pytest

from tools.reference.demod import AnalogDemod
from tools.reference.framer import encode_audio
from tools.reference.spectrum import Panadapter


def _rms(values: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.asarray(values, dtype=np.float64) ** 2)))


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


def test_ddc_rejects_oversized_native_output(vendor_required, monkeypatch):
    from tools.reference import sdk_bindings as sb
    from tools.reference.ddc import DdcChannel

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
    from tools.reference.filters import Agc
    agc = Agc(target=0.2)
    # FM discriminator output scale: several thousand, i.e. ~4 orders of magnitude off.
    y = agc.process(_agc_block(5000.0))
    assert np.max(np.abs(y)) <= 1.0, 'first block must not clip'
    assert 0.1 < _rms(y) < 0.4, 'first block should land near the target, not stay loud'


def test_agc_never_exceeds_the_peak_ceiling():
    from tools.reference.filters import Agc
    agc = Agc(target=0.2, ceiling=0.95)
    agc.process(_agc_block(0.2))                 # settle somewhere sensible
    for i, rms in enumerate((0.2, 3.0, 0.5, 12.0, 0.2)):
        y = agc.process(_agc_block(rms, seed=i + 1))
        assert np.max(np.abs(y)) <= 0.9500001, f'block {i} peak exceeded the ceiling'


def test_agc_hold_keeps_gain_but_still_bounds_the_peak():
    from tools.reference.filters import Agc
    agc = Agc(target=0.2)
    agc.process(_agc_block(0.2))
    gain_before = agc.gain
    y = agc.process(_agc_block(900.0, seed=7), hold=True)
    assert agc.gain == gain_before, 'hold must freeze the gain'
    assert np.max(np.abs(y)) <= 0.9500001


def test_agc_silence_does_not_raise_the_gain():
    from tools.reference.filters import Agc
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
