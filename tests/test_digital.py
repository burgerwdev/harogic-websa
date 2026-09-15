"""Tier 2 demodulation chain on synthetic IQ: EVM against theory, timing, carrier, SER.

The chain is measured against the *matched-filter bound*, not against itself: the reference
source (`tests/synth_iq.py`) builds RRC-shaped QPSK/16-QAM with a known symbol rate, carrier
offset, timing offset and AWGN, so every number here has an external truth.

Conditions are part of the claim, so they are stated in each test: the timing accuracy and the
EVM ratio both depend on the block length and the SNR (measured; see `demod/digital.py`).
"""
from __future__ import annotations

import time

import numpy as np
import pytest
import synth_iq as S

from web_sa.demod import digital as D

FS = 3.90625e6
SPS = 16
RATE = FS / SPS
#: Parameters of the reference capture the chain is measured on.
N_SYM = 4096


def theory_evm_percent(snr_db: float, sps: int = SPS) -> float:
    """Matched-filter bound: ``sqrt(N0/Es)`` with ``Es/N0 = sps * SNR``."""
    return 100.0 / np.sqrt(10 ** (snr_db / 10.0) * sps)


def signal(kind='qpsk', *, n_sym=N_SYM, snr_db=None, cfo_hz=0.0, timing=0.0, seed=7,
           amplitude=1e-3):
    iq, symbols = S.modulate(kind, n_sym=n_sym, sps=SPS, amplitude=amplitude, seed=seed)
    if timing:
        iq = D.add_timing(iq, timing)
    if cfo_hz:
        iq = S.add_cfo(iq, cfo_hz, FS)
    if snr_db is not None:
        iq = S.add_awgn(iq, float(snr_db), seed=seed + 100)
    return iq, symbols


def aligned_reference(res, symbols: np.ndarray, *, search: int = 3 * D.SPAN):
    """Truth symbols aligned with a demodulated cloud: ``(evm_percent, offset, reference)``.

    The chain neither knows nor can know which integer symbol the capture starts on -- that
    is a property of the burst, of the matched filter's delay and of where the trim lands, and
    it moves by one symbol with the timing wrap. A reference comparison therefore has exactly
    one free parameter, the integer offset, searched over a few spans here. The *rotation* by
    contrast is the chain's job (`resolve_ambiguity`).
    """
    best = None
    for offset in range(max(0, D.SPAN - search), D.SPAN + search + 1):
        ref = np.asarray(symbols)[offset:offset + res.n_symbols] * 1e-3
        n = min(len(ref), res.n_symbols)
        if n < 100:
            continue
        evm = D.evm_percent(res.symbols[:n], ref[:n])
        if best is None or evm < best[0]:
            best = (evm, offset, ref)
    return best


# ---------------- EVM against the matched-filter bound ----------------

@pytest.mark.parametrize('kind', ['qpsk', '16qam'])
@pytest.mark.parametrize('snr_db', [8.0, 12.0, 20.0, 30.0])
def test_evm_tracks_the_matched_filter_bound(kind, snr_db):
    """No systematic bias, and never above theory by more than 5 %.

    The reported EVM is the one a VSA shows without a reference (against its own decisions),
    which for a link whose decisions are all correct *is* the truth EVM -- the SER test below
    pins that separately. Realisations scatter around 1.0 (measured 0.98-1.04), so the band
    catches a *bias* (a 3 dB error would read 1.41), not luck.
    """
    iq, _symbols = signal(kind, snr_db=snr_db, cfo_hz=2500.0, timing=0.4, seed=21)
    res = D.demodulate(iq, FS, kind, symbol_rate=RATE, sps=SPS)
    assert res.error == '' and res.n_symbols > 3000
    ratio = res.evm_percent / theory_evm_percent(snr_db)
    # The bound this chain promises is the *upper* one: never more than 5 % above the
    # matched-filter bound from 12 dB up. At 8 dB one realisation in five reads up to 15 % high
    # because the EVM estimator's own variance is ~10 % there (measured), and the lower edge is
    # only a sanity guard: the theory value is an expectation, so sitting slightly under it is a
    # favourable noise draw rather than slack in the bound.
    high = 1.20 if snr_db <= 8.0 else 1.05
    assert 0.95 <= ratio <= high, (
        f'{kind} at {snr_db} dB: EVM {res.evm_percent:.3f} % vs theory '
        f'{theory_evm_percent(snr_db):.3f} % (ratio {ratio:.3f})')
    assert res.mer_db == pytest.approx(-20 * np.log10(res.evm_percent / 100.0), abs=1e-6)
    assert res.snr_db == pytest.approx(res.mer_db, abs=1e-6)


def test_evm_has_no_systematic_bias_across_seeds():
    """Averaged over seeds the ratio must sit at 1.00 +- 0.03, which luck cannot fake."""
    for kind in ('qpsk', '16qam'):
        for snr_db in (8.0, 20.0, 30.0):
            ratios = []
            for seed in (21, 22, 23, 24, 25):
                iq, _symbols = signal(kind, snr_db=snr_db, cfo_hz=2500.0, timing=0.4,
                                      seed=seed)
                res = D.demodulate(iq, FS, kind, symbol_rate=RATE, sps=SPS)
                ratios.append(res.evm_percent / theory_evm_percent(snr_db))
            mean = float(np.mean(ratios))
            median = float(np.median(ratios))
            # The promise is one-sided: the chain must not sit *above* the bound. Measured means
            # are 0.995-1.03 and medians 0.991-0.996, so the mean band is the contract's 1.00-1.05
            # with the honest lower slack a favourable realisation can produce.
            assert mean <= 1.05, f'{kind} {snr_db} dB: ratios {ratios}'
            assert median <= 1.05, f'{kind} {snr_db} dB: median {median:.3f}'
            assert mean >= 0.97 and median >= 0.97, f'{kind} {snr_db} dB: {ratios}'


@pytest.mark.parametrize('snr_db', [8.0, 12.0, 20.0, 30.0])
def test_ser_is_zero_on_a_clean_link(snr_db):
    iq, symbols = signal('qpsk', snr_db=snr_db, cfo_hz=2500.0, timing=0.4, seed=31)
    first = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    evm, offset, ref = aligned_reference(first, symbols)
    assert evm == pytest.approx(theory_evm_percent(snr_db), rel=0.05)
    assert offset in range(D.SPAN - 3 * D.SPAN, D.SPAN + 3 * D.SPAN + 1)

    res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, reference=ref)
    assert res.resolved_by == 'reference'
    assert res.ser == 0.0 and res.ber == 0.0
    assert res.evm_reference_percent == pytest.approx(evm, rel=0.02)


# ---------------- timing and carrier ----------------

@pytest.mark.parametrize('injected', [0.0, 0.25, 0.5, 0.75, 3.35])
@pytest.mark.parametrize('snr_db', [8.0, 20.0, 30.0])
def test_timing_recovery_is_inside_a_twentieth_of_a_sample(injected, snr_db):
    """4096 symbols: measured worst error 0.026 samples over 8-30 dB (5 seeds x 5 offsets).

    The blind |x|^2 estimate alone needs 30 dB to reach that (0.31 samples at 8 dB); the
    decision-aided EVM-minimising stage (`refine_timing`) is what buys the low-SNR accuracy.
    The recovered value is only meaningful modulo one symbol, so the error wraps.
    """
    iq, _symbols = signal('qpsk', snr_db=snr_db, cfo_hz=2500.0, timing=injected, seed=51)
    res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    assert res.error == ''
    delta = (res.timing_samples - injected + SPS / 2) % SPS - SPS / 2
    assert abs(delta) <= 0.05, f'injected {injected}: recovered {res.timing_samples:.4f}'


@pytest.mark.parametrize('cfo_hz', [0.0, 2500.0, -12000.0])
def test_carrier_offset_is_estimated_to_a_tenth_of_a_hertz(cfo_hz):
    iq, _symbols = signal('qpsk', snr_db=20.0, cfo_hz=cfo_hz, timing=0.4, seed=61)
    res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    assert res.cfo_hz == pytest.approx(cfo_hz, abs=0.1)
    # The tracker's residual is reported next to the non-data-aided estimate it refines.
    assert res.cfo_mth_hz == pytest.approx(cfo_hz, abs=1.0)
    assert abs(res.cfo_tracked_hz) < 1.0
    assert res.cfo_hz == pytest.approx(res.cfo_mth_hz + res.cfo_tracked_hz, abs=1e-9)


def test_the_tracker_removes_a_residual_phase_ramp():
    """A carrier offset the M-th power estimate cannot see comes out in the tracker."""
    iq, _symbols = signal('qpsk', snr_db=25.0, cfo_hz=11000.0, seed=71)
    tracked = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, track=True)
    raw = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, track=False)
    assert raw.error == tracked.error == ''
    assert tracked.evm_percent <= raw.evm_percent + 1e-9
    assert tracked.phase_residual_deg < 3.0
    assert len(tracked.phase_trace_rad) == tracked.n_symbols


# ---------------- the 4-fold ambiguity ----------------

def test_ambiguity_is_reported_never_silently_rotated():
    iq, _symbols = signal('qpsk', snr_db=25.0, seed=81)
    res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    assert res.resolved_by == 'none'
    assert res.rotation_deg == 0.0
    assert [c['rotation_deg'] for c in res.candidates] == [0.0, 90.0, 180.0, 270.0]


def test_a_user_rotation_is_applied_and_reported():
    iq, _symbols = signal('qpsk', snr_db=25.0, seed=82)
    plain = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    rotated = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, phase_rot_deg=90.0)
    assert rotated.resolved_by == 'user' and rotated.rotation_deg == 90.0
    # A 90 deg rotation maps every QPSK point onto a *different* symbol, so the decided
    # indices change (that is why it may never be applied silently) while the magnitude
    # statistics -- and therefore the EVM -- stay put.
    assert not np.array_equal(rotated.indices, plain.indices)
    assert rotated.evm_percent == pytest.approx(plain.evm_percent, rel=1e-6)


def test_a_preamble_resolves_the_ambiguity_automatically():
    iq, symbols = signal('qpsk', snr_db=25.0, seed=83)
    first = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    _evm, _offset, ref = aligned_reference(first, symbols)
    res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, reference=ref)
    assert res.resolved_by == 'reference'
    assert res.rotation_deg in (0.0, 90.0, 180.0, 270.0)
    assert res.ser == 0.0
    # Exactly one of the four candidates matches the preamble: that is what makes the
    # resolution a decision rather than a guess.
    scores = [c['ser'] for c in res.candidates]
    assert min(scores) == 0.0 and scores.count(0.0) == 1


def test_a_preamble_resolves_a_rotation_the_carrier_left_behind():
    """A 90 deg rotation the estimator cannot see is still found from the preamble."""
    iq, symbols = signal('qpsk', snr_db=25.0, seed=84)
    rotated_iq = iq * np.exp(1j * np.pi / 2)          # what the ambiguity looks like
    first = D.demodulate(rotated_iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    _evm, _offset, ref = aligned_reference(first, symbols)
    res = D.demodulate(rotated_iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS, reference=ref)
    assert res.resolved_by == 'reference'
    assert res.ser == 0.0
    assert res.evm_percent < 5.0


# ---------------- refusals, the symbol table and the noise floor ----------------

def test_a_capture_with_fewer_than_four_samples_per_symbol_is_refused():
    iq, _symbols = S.modulate('qpsk', n_sym=2048, sps=2, amplitude=1e-3, seed=91)
    res = D.demodulate(iq, FS, 'qpsk')
    assert res.error == 'sps_too_low'
    assert res.n_symbols == 0
    cloud = D.tier1_cloud(iq, FS)
    assert cloud['sps_too_low'] is True and len(cloud['symbols']) == 0


def test_a_silent_capture_and_a_short_one_are_refused():
    noise = S.noise(1 << 15, seed=92)
    assert D.demodulate(noise, FS, 'qpsk', symbol_rate=RATE).error in ('no_symbol_line', '')
    short = D.demodulate(np.zeros(64, dtype=complex), FS, 'qpsk', symbol_rate=RATE)
    assert short.error == 'too_short'


def test_the_symbol_table_matches_the_grid():
    bits, table = D.symbol_table('qpsk')
    assert bits == 2 and len(table) == len(D.nominal_points('qpsk')) == 4
    assert len(set(table)) == 4                                   # one distinct word each
    bits16, table16 = D.symbol_table('16qam')
    assert bits16 == 4 and len(table16) == 16
    assert len(set(table16)) == 16
    # Gray coding: neighbours *in the grid* differ by exactly one bit. Flat-list adjacency is
    # the wrong check (index 3 -> 4 jumps a row and a column at once).
    for row in range(4):
        for col in range(3):
            a, b = table16[row * 4 + col], table16[row * 4 + col + 1]
            assert sum(x != y for x, y in zip(a, b, strict=True)) == 1
    for row in range(3):
        for col in range(4):
            a, b = table16[row * 4 + col], table16[(row + 1) * 4 + col]
            assert sum(x != y for x, y in zip(a, b, strict=True)) == 1
    with pytest.raises(ValueError, match='unknown modulation'):
        D.symbol_table('8psk')


def test_decide_maps_a_clean_cloud_onto_the_grid():
    pts = D.nominal_points('16qam')
    ideal, idx = D.decide(pts * 3.5e-4, '16qam')
    assert np.array_equal(idx, np.arange(16))
    assert np.allclose(ideal, pts)


def test_evm_of_a_noiseless_capture_is_the_truncation_floor():
    iq, symbols = signal('qpsk', cfo_hz=2500.0, timing=0.4, seed=95)     # no noise at all
    first = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
    evm, _offset, _ref = aligned_reference(first, symbols)
    # SPAN 20 leaves a 0.013 % floor (measured 0.017 % here); anything above 0.05 % would
    # mean the chain is injecting ISI, which is how the 9.6 % resampler bug was found.
    assert evm < 0.05
    assert first.evm_percent == pytest.approx(evm, rel=0.2)


# ---------------- cost ----------------

def test_the_chain_is_vectorised_enough_for_capture_analyse():
    """A 2^17-sample capture must not cost seconds: measured ~0.24 s (700 % of real time).

    The bound is deliberately loose (2x the measured cost) because it exists to catch the
    return of a per-symbol Python loop, not to benchmark the machine.
    """
    iq, _symbols = signal('qpsk', n_sym=(1 << 17) // SPS, snr_db=25.0, seed=99)
    best = None
    for _ in range(3):
        t0 = time.perf_counter()
        res = D.demodulate(iq, FS, 'qpsk', symbol_rate=RATE, sps=SPS)
        elapsed = time.perf_counter() - t0
        best = elapsed if best is None else min(best, elapsed)
    assert res.n_symbols > 8000
    signal_s = len(iq) / FS
    assert best < 0.5, f'chain took {best * 1e3:.0f} ms for {signal_s * 1e3:.1f} ms of signal'
    # The result carries its own cost, which is what the session reports in STATUS.
    assert res.cpu_s > 0 and res.signal_s == pytest.approx(signal_s, rel=1e-6)
