#!/usr/bin/env python3
"""Offline DSP probe: Tier1 measurements and the Tier2 demodulation chain.

No hardware, no service state: synthetic IQ only (the tinySA cannot produce
PSK/QAM). Two questions:

Tier1 — are the vector measurements correct?  Power-vs-time on a burst,
CCDF against the Rayleigh reference, spectrum peak position against an injected
carrier offset, constellation cloud size against the noise floor, and the CPU
cost of each (as a fraction of real time).

Tier2 — does the in-house demodulator meet its theoretical EVM floor as SNR,
carrier offset, timing offset, roll-off and symbol rate change?  The reference
is the matched-filter bound ``EVM = sqrt(N0/Es) = 1/sqrt(sps * SNR)`` (validated
in probe_siggen_selfcheck.py), plus the noiseless floor from RRC truncation.

Usage:  python3 tools/vsa_probe/probe_dsp_loopback.py [--json out.json]
"""
from __future__ import annotations

import argparse
import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import demod as D  # noqa: E402
import measure as M  # noqa: E402
import siggen as S  # noqa: E402
from harness import Report  # noqa: E402

FS = 3.906e6          # a real decimate-16 IQS rate
SPS = 16
#: must be exactly FS/SPS: any mismatch makes the demodulator resample (time
#: stretch) and the symbol timing drifts across the frame (measured 9.6 % EVM
#: before this constant was fixed)
SYMBOL_RATE = FS / SPS       # 244.125 kSym/s
ROLLOFF = 0.35


def _sig(kind='qpsk', n_sym=4096, snr_db=None, cfo_rel=0.0, timing_samples=0.0,
         rolloff=ROLLOFF, sps=SPS, seed=3):
    sig = S.modulate(kind, n_sym=n_sym, sps=sps, rolloff=rolloff, seed=seed)
    iq = sig.iq
    if cfo_rel:
        iq = S.add_cfo(iq, cfo_rel * SYMBOL_RATE, FS)
    if timing_samples:
        iq = S.add_timing(iq, timing_samples)
    if snr_db is not None:
        iq = S.add_awgn(iq, snr_db, seed=seed + 7)
    return sig, iq


def tier1(rep: Report) -> None:
    sec = rep.section('Tier1 vector measurements')
    sig, iq = _sig('qpsk', n_sym=8192, snr_db=25.0)
    n = len(iq)

    # power vs time on a burst (50 % duty, 8 blocks on / 8 off)
    blk = 1024
    gate = (np.arange(n // blk) % 16) < 8
    burst = iq[:len(gate) * blk].reshape(-1, blk) * gate[:, None]
    burst = burst.reshape(-1)
    t0 = time.perf_counter()
    t, dbm, duty = M.power_vs_time(burst, FS, block=blk)
    cpu_pvt = time.perf_counter() - t0

    t0 = time.perf_counter()
    xdb, prob = M.ccdf(burst)
    cpu_ccdf = time.perf_counter() - t0

    t0 = time.perf_counter()
    freqs, spec = M.spectrum_dbm(iq, FS, nfft=4096)
    cpu_spec = time.perf_counter() - t0

    t0 = time.perf_counter()
    stft, _ = M.spectrogram(iq, FS, nfft=256, hop=128)
    cpu_stft = time.perf_counter() - t0

    t0 = time.perf_counter()
    con = M.constellation(iq, FS, rolloff=ROLLOFF, symbol_rate=SYMBOL_RATE, sps=SPS)
    cpu_con = time.perf_counter() - t0

    # carrier offset seen in the spectrum: the power-weighted centroid of a
    # symmetric modulation spectrum is its centre (an argmax would just find a
    # random bin of a noise-like spectrum). Averaged over seeds because a single
    # noise realisation tilts the centroid by ~1 kHz at 25 dB SNR.
    cfo_rows = []
    for snr in (10.0, 20.0, 30.0, 40.0):
        errs = []
        for seed in (1, 2, 3):
            _, iq2 = _sig('qpsk', n_sym=4096, snr_db=snr, cfo_rel=0.01, seed=seed)
            f, s = M.spectrum_dbm(iq2, FS, nfft=4096)
            c = M.spectral_centroid(f, s, band_hz=0.75 * SYMBOL_RATE)
            errs.append(c - 0.01 * SYMBOL_RATE)
        cfo_rows.append([snr, round(float(np.mean(errs)), 1),
                         round(float(np.std(errs)), 1)])
    rep.table(['SNR dB', 'centroid error mean Hz', 'stdev Hz'], cfo_rows, sec)
    rep.kv('injected CFO for the centroid sweep Hz', round(0.01 * SYMBOL_RATE, 1))

    sig_cloud = con['symbols']
    rows = [
        ['power vs time', n / FS, cpu_pvt, 100 * cpu_pvt / (n / FS), 1e11 * cpu_pvt / n],
        ['CCDF', n / FS, cpu_ccdf, 100 * cpu_ccdf / (n / FS), 1e11 * cpu_ccdf / n],
        ['spectrum (Welch 4096)', n / FS, cpu_spec, 100 * cpu_spec / (n / FS),
         1e11 * cpu_spec / n],
        ['spectrogram 256/128', n / FS, cpu_stft, 100 * cpu_stft / (n / FS),
         1e11 * cpu_stft / n],
        ['constellation (Tier1)', n / FS, cpu_con, 100 * cpu_con / (n / FS),
         1e11 * cpu_con / n],
    ]
    rep.table(['measurement', 'signal s', 'CPU s', 'CPU % realtime', 'us/100k'],
              rows, sec)
    rep.kv('burst duty measured', round(duty, 3))
    rep.kv('burst duty expected', 0.5)
    rep.kv('power-vs-time points', len(t))
    rep.kv('CCDF points', len(xdb))
    rep.kv('constellation symbols', len(sig_cloud) if sig_cloud is not None else 0)
    rep.kv('constellation symbol rate error Hz',
           round(con['symbol_rate_est'] - SYMBOL_RATE, 2))
    # Rayleigh CCDF check on noise only, single samples
    rng = np.random.default_rng(11)
    noise = (rng.standard_normal(len(iq)) + 1j * rng.standard_normal(len(iq))) / np.sqrt(2)
    xdb_n, p_n = M.ccdf(noise, block=1)
    err = float(np.max(np.abs(p_n - M.ccdf_rayleigh(xdb_n))))
    rep.kv('noise CCDF max |measured - Rayleigh|', round(err, 4))
    del sig, sig_cloud


def tier2_snr(rep: Report) -> None:
    sec = rep.section('Tier2 EVM vs SNR (244.14 kSym/s, sps 16, roll-off 0.35)')
    rows = []
    for kind in ('qpsk', '16qam'):
        for snr in (8.0, 12.0, 16.0, 20.0, 25.0, 30.0):
            sig, iq = _sig(kind, n_sym=4096, snr_db=snr)
            r = D.demodulate(iq, FS, kind=kind, rolloff=ROLLOFF,
                             symbol_rate=SYMBOL_RATE, sps=SPS, truth=sig.symbols)
            theory = S.theoretical_evm_percent(snr, SPS)
            rows.append([kind, snr, round(r.evm_vs_truth, 3), round(theory, 3),
                         round(r.evm_vs_truth / theory, 3),
                         round(r.evm_vs_sliced, 3), round(r.ser_vs_truth, 5)])
    rep.table(['kind', 'SNR dB', 'EVM % (truth)', 'EVM % theory', 'ratio',
               'EVM % (sliced)', 'SER'], rows, sec)
    ratios = [r[4] for r in rows]
    rep.kv('EVM/theory range', f'{min(ratios):.3f} .. {max(ratios):.3f}')


def tier2_impairments(rep: Report) -> None:
    sec = rep.section('Tier2 EVM vs carrier offset, timing offset, roll-off')
    rows = []
    for rel in (0.0, 0.0005, 0.002, 0.01, 0.05):
        sig, iq = _sig('qpsk', n_sym=4096, snr_db=25.0, cfo_rel=rel)
        r = D.demodulate(iq, FS, kind='qpsk', rolloff=ROLLOFF,
                         symbol_rate=SYMBOL_RATE, sps=SPS, truth=sig.symbols)
        rows.append(['cfo', f'{rel * SYMBOL_RATE:.1f} Hz', round(r.cfo_hz, 1),
                     round(r.evm_vs_truth, 3), r.ser_vs_truth])
    for off in (0.0, 0.25, 0.5, 0.75):
        sig, iq = _sig('qpsk', n_sym=4096, snr_db=25.0, timing_samples=off)
        r = D.demodulate(iq, FS, kind='qpsk', rolloff=ROLLOFF,
                         symbol_rate=SYMBOL_RATE, sps=SPS, truth=sig.symbols)
        rows.append(['timing', f'{off} samples', round(r.timing_samples, 3),
                     round(r.evm_vs_truth, 3), r.ser_vs_truth])
    for rolloff in (0.1, 0.25, 0.35, 0.5, 0.9):
        sig, iq = _sig('16qam', n_sym=4096, snr_db=25.0, rolloff=rolloff)
        r = D.demodulate(iq, FS, kind='16qam', rolloff=rolloff,
                         symbol_rate=SYMBOL_RATE, sps=SPS, truth=sig.symbols)
        rows.append(['roll-off', f'{rolloff}', round(r.symbol_rate, 1),
                     round(r.evm_vs_truth, 3), r.ser_vs_truth])
    rep.table(['impairment', 'value', 'estimated', 'EVM % (truth)', 'SER'],
              rows, sec)
    rep.kv('EVM floor expectation (theory at 25 dB)', S.theoretical_evm_percent(25, SPS))


def tier2_rate(rep: Report) -> None:
    sec = rep.section('Tier2 behaviour across symbol rate / sps / bandwidth')
    rows = []
    for rate, sps in ((61.035e3, 16), (244.140625e3, 16), (976.5625e3, 8),
                      (976.5625e3, 4), (244.140625e3, 8)):
        fs = rate * sps
        sig = S.modulate('16qam', n_sym=2048, sps=sps, rolloff=0.35, seed=5)
        iq = S.add_awgn(sig.iq, 25.0, seed=9)
        r = D.demodulate(iq, fs, kind='16qam', rolloff=0.35, sps=sps,
                         truth=sig.symbols)
        rows.append([f'{rate / 1e3:.3f}k', sps, f'{fs / 1e6:.4f}M',
                     round(r.symbol_rate / 1e3, 3), round(r.evm_vs_truth, 3),
                     round(S.theoretical_evm_percent(25.0, sps), 3),
                     round(r.cpu_s, 4), round(r.cpu_s / r.signal_s * 100, 2)])
    rep.table(['symbol rate', 'sps', 'fs', 'rate est kSym/s', 'EVM %',
               'theory %', 'CPU s', 'CPU % realtime'], rows, sec)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--json', default=os.path.join(os.path.dirname(__file__),
                                                   'dsp_loopback.json'))
    args = ap.parse_args()

    rep = Report('offline DSP loopback: Tier1 measurements + Tier2 demodulation')
    tier1(rep)
    tier2_snr(rep)
    tier2_impairments(rep)
    tier2_rate(rep)
    rep.save(args.json)
    print('\nDONE', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
