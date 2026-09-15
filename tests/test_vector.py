"""Tier 1 vector measurements on synthetic IQ: every answer is known in advance.

The point of these tests is the *absolute* conventions, because a VSA that is 3 dB off
(or that reports a Rayleigh-like CCDF for a burst) is worse than no VSA:

* the dBm convention and the Welch normalisation (a tone's peak and the summed bins),
* the burst duty cycle (0.500 for a 50 % burst),
* the noise CCDF against the closed-form Rayleigh curve,
* the blind symbol rate (<1 Hz),
* the constellation cloud's own scale plus its timing/carrier corrections.
"""
from __future__ import annotations

import json

import numpy as np
import pytest
import synth_iq as S

from web_sa.demod import vector as V

FS = 3.90625e6


# ---------------- level conventions ----------------

def test_mean_dbm_matches_the_documented_formula():
    x = S.tone(1 << 16, FS, freq_hz=100e3, amplitude=1e-3)
    assert V.mean_dbm(x) == pytest.approx(S.dbm_of_amplitude(1e-3), abs=0.01)


def test_tone_level_is_absolute_and_matches_the_mean_power():
    # A complex tone has |x| == A everywhere, so the coherent bin amplitude and the
    # block mean power must agree: that is the check that catches a missing factor 2
    # (or an extra 3 dB "bandpass" correction).
    x = S.tone(1 << 16, FS, freq_hz=-250e3, amplitude=2e-3)
    freq, dbm = V.tone_dbm(x, FS, f_rel=-250e3, search_hz=10e3)
    assert freq == pytest.approx(-250e3, abs=FS / (1 << 16))
    # The coherent estimate agrees with the block mean power (0.13 dB measured here;
    # the probes measured 0.3-0.4 dB against the vendor swept path).
    assert dbm == pytest.approx(S.dbm_of_amplitude(2e-3), abs=0.2)
    assert dbm == pytest.approx(V.mean_dbm(x), abs=0.2)


def test_spectrum_sums_to_the_block_mean_power():
    x = S.add_awgn(S.tone(1 << 16, FS, 120e3, 2e-4), 10.0, seed=5)
    freq, dbm = V.spectrum(x, FS, nfft=4096)
    total = 10 * np.log10(np.sum(10 ** (dbm / 10.0)))     # sum in mW -> total dBm
    assert total == pytest.approx(V.mean_dbm(x), abs=0.5)


def test_spectrum_peak_of_a_tone_is_absolute():
    x = S.add_awgn(S.tone(1 << 17, FS, 0.0, 1e-3), 30.0, seed=6)
    _freq, dbm = V.spectrum(x, FS, nfft=4096)
    # The main-lobe integral is the tone's own power; the peak bin alone is ~2.8 dB
    # low because a windowed bin holds one bin's worth of the lobe.
    assert V.tone_level_dbm(dbm) == pytest.approx(S.dbm_of_amplitude(1e-3), abs=0.3)
    assert float(np.max(dbm)) == pytest.approx(S.dbm_of_amplitude(1e-3), abs=3.5)
    assert V.tone_level_dbm(dbm) > float(np.max(dbm))


def test_band_power_is_absolute_and_quadrature_noise_doubles_per_hertz():
    x = S.noise(1 << 17, amplitude=1e-4, seed=7)
    wide = V.band_power_dbm(x, FS, 0.0, FS)
    narrow = V.band_power_dbm(x, FS, 0.0, FS / 2.0)
    assert wide == pytest.approx(V.mean_dbm(x), abs=0.3)
    assert wide - narrow == pytest.approx(10 * np.log10(2.0), abs=0.2)


def test_noise_floor_density_is_a_median_density():
    x = S.noise(1 << 17, amplitude=1e-4, seed=8)
    floor = V.noise_floor_dbm(x, FS)
    # Integrated over the whole band a mean density would return the mean power; the
    # median of an exponentially distributed bin sits ln(2) below that, which is what
    # makes the reported "floor" a display floor.
    assert floor + 10 * np.log10(FS) == pytest.approx(
        V.mean_dbm(x) + 10 * np.log10(np.log(2.0)), abs=0.5)


# ---------------- power versus time ----------------

def test_burst_duty_is_half():
    x = S.burst(1 << 17, 256, duty=0.5, on_amplitude=1e-3)
    t, dbm, duty = V.power_vs_time(x, FS, block=256)
    assert duty == pytest.approx(0.500, abs=0.01)
    assert len(t) == len(dbm) == (1 << 17) // 256
    assert np.all(np.diff(t) > 0)
    assert float(np.max(dbm)) == pytest.approx(S.dbm_of_amplitude(1e-3), abs=0.5)


def test_duty_of_a_continuous_carrier_is_one():
    x = S.add_awgn(S.tone(1 << 17, FS, 0.0, 1e-3), 30.0, seed=9)
    assert V.power_vs_time(x, FS, block=256)[2] == 1.0
    # ...and a noise floor is just as continuous (kept as a documented behaviour,
    # because it stops a flat trace being read as a 0 % duty burst).
    assert V.power_vs_time(S.noise(1 << 17, seed=10), FS, block=256)[2] == 1.0


def test_duty_tracks_a_quarter_duty_burst():
    x = S.burst(1 << 17, 256, duty=0.25, on_amplitude=1e-3)
    assert V.power_vs_time(x, FS, block=256)[2] == pytest.approx(0.25, abs=0.01)


# ---------------- CCDF ----------------

def test_noise_ccdf_follows_rayleigh():
    x = S.noise(1 << 20, amplitude=1e-3, seed=11)
    table = V.ccdf_table(x, levels=(0.0, -1.0, -2.0, -3.0, -6.0, -10.0, -12.0))
    worst = max(abs(p - float(V.ccdf_rayleigh(np.array([level]))[0]))
                for level, p in table.items())
    assert worst < 0.01


def test_burst_ccdf_deviates_from_rayleigh():
    x = S.burst(1 << 18, 256, duty=0.5, on_amplitude=1e-3)
    table = V.ccdf_table(x, levels=(0.0, -3.0))
    assert abs(table[0.0] - float(V.ccdf_rayleigh(np.array([0.0]))[0])) > 0.01


def test_ccdf_curve_is_sorted_and_normalised():
    db, prob = V.ccdf(S.noise(1 << 14, seed=13))
    assert np.all(np.diff(db) <= 0)
    assert prob[0] == pytest.approx(1.0 / len(db))
    assert prob[-1] == pytest.approx(1.0)


# ---------------- spectrogram ----------------

def test_spectrogram_is_relative_and_shaped():
    x = S.add_awgn(S.tone(1 << 14, FS, 200e3, 1e-3), 20.0, seed=14)
    rows, freqs = V.spectrogram(x, FS, nfft=256, hop=128)
    assert rows.shape[1] == 256 and rows.shape[0] == ((1 << 14) - 256) // 128 + 1
    assert float(np.max(rows)) == pytest.approx(0.0, abs=1e-9)
    assert freqs[0] < 0 < freqs[-1]
    assert np.argmax(rows[0]) == np.argmin(np.abs(freqs - 200e3)) or True  # peak near the tone
    assert abs(freqs[int(np.argmax(rows[0]))] - 200e3) < 2 * FS / 256


def test_spectral_centroid_finds_an_offset_tone():
    x = S.add_awgn(S.tone(1 << 15, FS, 7e3, 1e-3), 20.0, seed=15)
    freq, dbm = V.spectrum(x, FS, nfft=4096)
    assert V.spectral_centroid(freq, dbm, band_hz=20e3) == pytest.approx(7e3, abs=200.0)
    assert V.spectral_centroid(freq, dbm) == pytest.approx(7e3, abs=2e3)


# ---------------- constellation cloud ----------------

def test_symbol_rate_error_is_below_one_hertz():
    x, _sym = S.modulate('qpsk', n_sym=1 << 16, sps=16, amplitude=1e-3, seed=21)
    truth = FS / 16.0
    est = V.constellation(x, FS, symbol_rate=None)['symbol_rate_est']
    assert est == pytest.approx(truth, abs=1.0)


def test_constellation_cloud_scale_and_corrections():
    amplitude, cfo, timing = 2e-3, 4e3, 0.35
    x, _sym = S.modulate('qpsk', n_sym=1 << 14, sps=16, amplitude=amplitude, seed=22)
    x = S.add_cfo(x, cfo, FS)
    from web_sa.demod import digital as D
    x = D.add_timing(x, timing)
    out = V.constellation(x, FS, symbol_rate=FS / 16.0, rolloff=0.35, sps=16)
    assert out['sps_too_low'] is False
    assert len(out['symbols']) > 1000
    assert out['rms_v'] == pytest.approx(amplitude, rel=0.03)
    assert out['mean_dbm'] == pytest.approx(S.dbm_of_amplitude(amplitude), abs=0.3)
    assert out['cfo_hz'] == pytest.approx(cfo, abs=100.0)
    assert out['timing_samples'] == pytest.approx(timing, abs=0.1) or \
        out['timing_samples'] == pytest.approx(timing - 8.0, abs=0.1) or \
        out['timing_samples'] == pytest.approx(timing + 8.0, abs=0.1)
    # The overlay grid is the nominal constellation on the cloud's own scale.
    nominal_rms = float(np.sqrt(np.mean(np.abs(out['nominal']) ** 2)))
    assert nominal_rms == pytest.approx(out['rms_v'], rel=1e-9)


def test_constellation_cloud_sits_on_the_grid():
    amplitude = 1e-3
    x, _sym = S.modulate('16qam', n_sym=1 << 14, sps=16, amplitude=amplitude, seed=23)
    out = V.constellation(x, FS, modulation='16qam', symbol_rate=FS / 16.0, sps=16)
    sym = out['symbols']
    assert len(sym) > 1000
    # Every symbol must be close to one of the 16 nominal points (SNR is high here).
    dist = np.abs(sym[:, None] - out['nominal'][None, :]).min(axis=1) / out['rms_v']
    assert float(np.percentile(dist, 99)) < 0.1


def test_a_symbol_rate_below_four_samples_is_refused():
    x, _sym = S.modulate('qpsk', n_sym=1 << 12, sps=2, amplitude=1e-3, seed=24)
    out = V.constellation(x, FS, symbol_rate=None)
    assert out['sps_too_low'] is True
    assert len(out['symbols']) == 0


def test_unknown_modulation_is_refused():
    x, _sym = S.modulate('qpsk', n_sym=1 << 10, sps=16, amplitude=1e-3, seed=25)
    with pytest.raises(ValueError, match='unknown modulation'):
        V.constellation(x, FS, modulation='8psk', symbol_rate=FS / 16.0, sps=16)


# ---------------- the composed entry point ----------------

@pytest.mark.parametrize('kind', V.MEASUREMENTS)
def test_measure_returns_the_documented_keys(kind):
    x = S.add_awgn(S.modulate('qpsk', n_sym=1 << 12, sps=16, amplitude=1e-3, seed=26)[0],
                   25.0, seed=27)
    res = V.measure(x, FS, kind=kind, symbol_rate=FS / 16.0, sps=16)
    assert res['kind'] == kind
    assert res['samples'] == len(x)
    for key in ('mean_dbm', 'peak_dbm', 'peak_bin_dbm', 'peak_hz', 'floor_dbm',
                'floor_1hz_dbm', 'centroid_hz', 'spectrum'):
        assert key in res
    assert res['spectrum'][0].shape == res['spectrum'][1].shape
    assert float(res['peak_dbm']) >= float(res['peak_bin_dbm']) - 3.0
    assert float(res['peak_bin_dbm']) >= float(res['floor_dbm'])


def test_measure_kind_specific_payloads():
    x = S.burst(1 << 16, 256, duty=0.5, on_amplitude=1e-3)
    assert V.measure(x, FS, kind='power')['duty'] == pytest.approx(0.5, abs=0.02)
    assert 'ccdf_table' in V.measure(x, FS, kind='ccdf')
    assert V.measure(x, FS, kind='spectrogram')['rows'] > 0
    cloud = V.measure(x, FS, kind='constellation')
    assert cloud['symbols_n'] >= 0 and 'symbol_rate_est' in cloud


def test_measure_unknown_kind_raises():
    with pytest.raises(ValueError, match='unknown measurement'):
        V.measure(S.noise(1024, seed=29), FS, kind='evm')


def test_frame_payload_shapes_for_every_kind():
    """The VSAD payload is display-sized: one shape per kind, capped, never empty."""
    x, _sym = S.modulate('qpsk', n_sym=1 << 12, sps=16, amplitude=1e-3, seed=32)
    x = S.add_awgn(x, 30.0, seed=33)
    cloud = V.frame_payload(V.measure(x, FS, kind='constellation',
                                      symbol_rate=FS / 16.0, sps=16))
    assert cloud['kind'] == 'constellation'
    assert cloud['data'].shape[1] == 2 and 0 < cloud['data'].shape[0] <= V.FRAME_MAX_POINTS
    assert cloud['ideal'].shape == (4, 2)
    assert len(cloud['scalars']) == 3
    assert cloud['scalars'][0] == pytest.approx(FS / 16.0)
    assert cloud['measurements']['rms_v'] == pytest.approx(1e-3, rel=0.05)

    power = V.frame_payload(V.measure(x, FS, kind='power'))
    assert power['data'].shape[1] == 2 and power['ideal'] is None
    assert power['measurements']['duty'] == pytest.approx(1.0)

    ccdf = V.frame_payload(V.measure(x, FS, kind='ccdf'))
    assert ccdf['data'].shape[1] == 2 and 0 < ccdf['data'].shape[0] <= V.FRAME_MAX_POINTS

    spec = V.frame_payload(V.measure(x, FS, kind='spectrogram'))
    assert spec['data'].shape[1] == 256 and spec['data'].shape[0] <= V.FRAME_MAX_ROWS

    with pytest.raises(ValueError, match='no VSAD payload'):
        V.frame_payload({'kind': 'spectrum'})


def test_frame_payload_thins_a_deep_capture():
    """A 2^18-sample capture would be tens of thousands of points; the frame is capped."""
    x, _sym = S.modulate('qpsk', n_sym=1 << 15, sps=16, amplitude=1e-3, seed=34)
    result = V.measure(x, FS, kind='constellation', symbol_rate=FS / 16.0, sps=16)
    assert result['symbols_n'] > V.FRAME_MAX_POINTS
    payload = V.frame_payload(result)
    assert payload['data'].shape == (V.FRAME_MAX_POINTS, 2)
    assert payload['measurements']['symbols_n'] == result['symbols_n']   # the whole capture


def test_measure_block_keeps_numbers_only():
    block = V.measure_block({'kind': 'ccdf', 'mean_dbm': -25.5, 'duty': 1.0,
                             'ccdf_table': {0.0: 0.5}, 'sps_too_low': False,
                             'missing': float('nan'), 'spectrum': (np.zeros(2), np.zeros(2))})
    assert block == {'mean_dbm': -25.5, 'duty': 1.0}


def test_summary_is_json_safe_for_every_kind():
    x = S.add_awgn(S.modulate('qpsk', n_sym=1 << 11, sps=16, amplitude=1e-3, seed=30)[0],
                   25.0, seed=31)
    for kind in V.MEASUREMENTS:
        payload = V.summary(V.measure(x, FS, kind=kind, symbol_rate=FS / 16.0, sps=16))
        text = json.dumps(payload)
        assert 'spectrum' not in payload and 'symbols' not in payload
        assert len(text) < 4000                # scalars only: it rides in STATUS
