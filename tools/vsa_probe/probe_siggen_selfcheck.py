#!/usr/bin/env python3
"""Self-check of the synthetic IQ generator (offline, no hardware, no device).

Proves the generator can be trusted as the reference for every synthetic claim
in the VSA analysis: the noiseless matched-filter EVM must be ~0, AWGN must hit
the requested SNR and the measured EVM must match sqrt(Pn/Ps), a timing offset
must appear exactly where it was injected, and a carrier offset must be
recoverable as a per-symbol phase increment.

Usage:  python3 tools/vsa_probe/probe_siggen_selfcheck.py [--json out.json]
"""
from __future__ import annotations

import argparse
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import siggen as S  # noqa: E402
from harness import Report  # noqa: E402

KINDS = ('bpsk', 'qpsk', '8psk', '16qam', '64qam')
SPS = 8
ROLLOFF = 0.35


def check_constellations(rep: Report) -> None:
    sec = rep.section('constellation normalisation')
    rows = []
    for k in KINDS:
        p = float(np.mean(np.abs(S.constellation(k)) ** 2))
        rows.append([k, len(S.constellation(k)), S.bits_per_symbol(k), p])
    rep.table(['kind', 'points', 'bits/sym', 'mean power'], rows, sec)
    worst = max(abs(r[3] - 1.0) for r in rows)
    rep.kv('max |mean power - 1|', worst)
    assert worst < 1e-12, 'constellations are not unit average power'


def check_noiseless(rep: Report) -> None:
    sec = rep.section('noiseless loopback (matched filter, exact timing)')
    rows = []
    for k in KINDS:
        sig = S.modulate(k, n_sym=1024, sps=SPS, rolloff=ROLLOFF, seed=7)
        raw = S.matched_filter_symbols(sig.iq, SPS, ROLLOFF)
        ref = sig.ref_from(raw)
        rx = S.align_gain(raw[:len(ref)], ref)
        evm = S.evm_rms_percent(rx, ref)
        ser = S.symbol_errors(rx, ref, k)
        rows.append([k, round(float(np.mean(np.abs(sig.iq[400:-400]) ** 2)), 6), evm, ser])
    rep.table(['kind', 'tx power', 'EVM %', 'symbol err'], rows, sec)
    worst = max(r[2] for r in rows)
    rep.kv('worst noiseless EVM %', worst)
    assert worst < 0.05, 'noiseless loopback is not clean'
    assert max(r[3] for r in rows) == 0.0, 'noiseless decisions are not exact'


def check_span_floor(rep: Report) -> None:
    """The noiseless EVM floor is the RRC truncation span, not numerical error."""
    sec = rep.section('measured noiseless EVM floor vs RRC span')
    rows = []
    for span in (4, 10, 20, 64):
        sig = S.modulate('qpsk', n_sym=4096, sps=SPS, rolloff=ROLLOFF, seed=7, span=span)
        raw = S.matched_filter_symbols(sig.iq, SPS, ROLLOFF, span=span)
        ref = sig.ref_from(raw)
        rx = S.align_gain(raw[:len(ref)], ref)
        rows.append([span, S.evm_rms_percent(rx, ref)])
    rep.table(['span (symbols)', 'EVM %'], rows, sec)
    assert rows[-1][1] < rows[0][1], 'the EVM floor must fall with a longer span'


def check_awgn(rep: Report) -> None:
    sec = rep.section('AWGN: realized SNR and EVM vs theory')
    rows = []
    for k in ('qpsk', '16qam'):
        for snr in (10.0, 15.0, 20.0, 25.0, 30.0):
            sig = S.modulate(k, n_sym=4096, sps=SPS, rolloff=ROLLOFF, seed=11)
            noisy = S.add_awgn(sig.iq, snr, seed=100 + int(snr))
            real = S.realized_snr_db(sig.iq, noisy)
            raw = S.matched_filter_symbols(noisy, SPS, ROLLOFF)
            ref = sig.ref_from(raw)
            rx = S.align_gain(raw[:len(ref)], ref)
            evm = S.evm_rms_percent(rx, ref)
            th = S.theoretical_evm_percent(snr, SPS)
            rows.append([k, snr, round(real, 3), evm, th, evm / th])
    rep.table(['kind', 'SNR req', 'SNR real', 'EVM %', 'EVM theory', 'ratio'],
              rows, sec)
    worst_snr = max(abs(r[2] - r[1]) for r in rows)
    ratios = [r[5] for r in rows]
    rep.kv('max |realized SNR - requested| dB', round(worst_snr, 3))
    rep.kv('EVM/theory range', f'{min(ratios):.3f} .. {max(ratios):.3f}')
    assert worst_snr < 0.1, 'AWGN does not hit the requested SNR'
    assert min(ratios) > 0.9 and max(ratios) < 1.1, 'EVM does not track theory'


def check_timing(rep: Report) -> None:
    sec = rep.section('timing offset (injected vs measured)')
    sig = S.modulate('qpsk', n_sym=2048, sps=SPS, rolloff=ROLLOFF, seed=13)
    rows = []
    for want in (0.0, 0.25, 0.5, 0.75):
        shifted = S.add_timing(sig.iq, want)
        # scan the recovered timing phase and keep the best EVM
        scored = []
        for ph in np.arange(0, SPS, 1):
            raw = S.matched_filter_symbols(shifted, SPS, ROLLOFF, timing=ph)
            ref = sig.ref_from(raw)
            scored.append((S.evm_rms_percent(S.align_gain(raw[:len(ref)], ref), ref), ph))
        best = min(scored)
        rows.append([want, best[1], best[0]])
    rep.table(['injected offset (samples)', 'recovered phase', 'best EVM %'],
              rows, sec)
    # the integer scan leaves at most half a sample of residual timing error
    err = max(abs(((r[1] - r[0]) + SPS / 2) % SPS - SPS / 2) for r in rows)
    rep.kv('worst residual timing error (samples)', round(err, 3))
    assert err <= 0.5 + 1e-9, 'injected timing offset is not recovered'
    assert rows[0][2] < 0.05, 'zero timing offset should be clean'
    # sub-sample timing error is the measured EVM cost of timing uncertainty
    rep.kv('EVM % at 0.5-sample timing offset', rows[2][2])


def check_cfo(rep: Report) -> None:
    sec = rep.section('carrier offset (injected vs measured)')
    fs = float(SPS)                     # sample rate in "samples/s", symbol rate = 1
    sig = S.modulate('qpsk', n_sym=4096, sps=SPS, rolloff=ROLLOFF, seed=17)
    rows = []
    for cfo in (0.0, 0.001, 0.005, 0.02):
        off = S.add_cfo(sig.iq, cfo, fs)
        raw = S.matched_filter_symbols(off, SPS, ROLLOFF)
        ref = sig.ref_from(raw)
        rx = raw[:len(ref)]
        # data-independent phase increment: divide the differential product by
        # the reference differential product (constant-modulus QPSK)
        d = float(np.angle(np.mean(rx[1:] * np.conj(rx[:-1])
                                   * np.conj(ref[1:]) * ref[:-1])))
        est = d / (2 * np.pi) * fs
        # correct the estimated offset and measure the residual EVM
        corr = S.align_gain(rx * np.exp(-1j * d * np.arange(len(rx))), ref)
        rows.append([cfo, round(est / fs, 6), S.evm_rms_percent(corr, ref)])
    rep.table(['CFO (cyc/sample)', 'estimated (cyc/sample)', 'EVM % after correction'],
              rows, sec)
    worst = max(abs(r[1] - r[0]) for r in rows)
    rep.kv('worst CFO estimate error (cyc/sample)', worst)
    assert worst < 1e-4, 'CFO is not measurable from the generated signal'
    assert max(r[2] for r in rows) < 1.0, 'estimated CFO does not correct the data'


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--json', default=os.path.join(os.path.dirname(__file__),
                                                   'siggen_selfcheck.json'))
    args = ap.parse_args()

    rep = Report('synthetic IQ generator self-check (offline)')
    check_constellations(rep)
    check_noiseless(rep)
    check_span_floor(rep)
    check_awgn(rep)
    check_timing(rep)
    check_cfo(rep)
    rep.save(args.json)
    print('\nSELF-CHECK PASSED', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
