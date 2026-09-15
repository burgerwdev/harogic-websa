#!/usr/bin/env python3
"""Hardware probe: IQS capture capability of the SAN-90 for a VSA mode.

Measures the two acquisition models a VSA could use:

* ``framed``  — FixedPoints: one trigger == one finite frame, reassembled from
  its packets (trigger latency, packet count, shortfall, spectral continuity,
  repeatability). This is the capture-then-analyse path.
* ``depth``   — how deep a FixedPoints frame can go at each DecimateFactor
  before configuration or fetch fails.
* ``stream``  — Adaptive: sustained continuous rate, packet loss and gap
  statistics. This is the streaming path.

Every run prints a summary and writes JSON. The tinySA Ultra supplies a CW tone
whose exact frequency/level is recorded with each number.

Usage (WebSA is stopped automatically and restored afterwards):
    python3 tools/vsa_probe/probe_iq_capture.py all
    python3 tools/vsa_probe/probe_iq_capture.py framed --decimate 8 --trigger-length 262144 --repeat 5
    python3 tools/vsa_probe/probe_iq_capture.py depth --decimates 4,16,64
    python3 tools/vsa_probe/probe_iq_capture.py stream --decimates 4,16,64 --seconds 5
"""
from __future__ import annotations

import argparse
import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analysis as A  # noqa: E402
import capture as C  # noqa: E402
from harness import DeviceSession, Report, exclusive_websa, tiny_sa  # noqa: E402

CENTER = 100e6
TONE_OFFSET = 200e3          # CW tone 200 kHz above the IQS centre
TONE_DBM = -25.0
TONE_FREQ = CENTER + TONE_OFFSET


def _conditions(rep: Report, cfg: C.CaptureConfig, extra: dict | None = None) -> None:
    sec = rep.section('bench conditions')
    for k, v in cfg.bench().items():
        sec[k] = v
        print(f'  {k:<34s} {v}', flush=True)
    sec['tone_freq_hz'] = TONE_FREQ
    sec['tone_dbm'] = TONE_DBM
    print(f'  {"tone_freq_hz":<34s} {TONE_FREQ}', flush=True)
    print(f'  {"tone_dbm":<34s} {TONE_DBM}', flush=True)
    print(f'  {"cable/attenuation":<34s} direct SMA, no external attenuator',
          flush=True)
    for k, v in (extra or {}).items():
        sec[k] = v


def cmd_framed(rep: Report, dev, repeat: int, cfg: C.CaptureConfig) -> None:
    sec = rep.section('FixedPoints frame capture')
    rows = []
    spectra = []
    for i in range(repeat):
        r = C.framed_capture(dev.dev, dev.T, cfg)
        if r.iq is None or not len(r.iq):
            rows.append([i, 0, r.packets_ok, r.packets_err, r.first_status, 0, 0])
            continue
        f, dbm = A.tone_dbm(r.iq, r.fs, TONE_OFFSET, search_hz=20e3)
        spectra.append((f, dbm, A.mean_dbm(r.iq)))
        rows.append([i, len(r.iq), r.packets_ok, r.packets_err, r.first_status,
                     round(f, 1), round(dbm, 2)])
    rep.table(['run', 'samples', 'pkts ok', 'pkts err', 'status', 'peak Hz',
               'peak dBm'], rows, sec)
    if spectra:
        arr = np.array(spectra, dtype=float)
        sec['repeat_count'] = len(spectra)
        rep.kv('peak frequency mean Hz', float(arr[:, 0].mean()))
        rep.kv('peak frequency stdev Hz', float(arr[:, 0].std()))
        rep.kv('peak power mean dBm', float(arr[:, 1].mean()))
        rep.kv('peak power stdev dB', float(arr[:, 1].std()))
        rep.kv('expected offset Hz', TONE_OFFSET)
        rep.kv('peak offset error Hz', float(arr[:, 0].mean() - TONE_OFFSET))
    r0 = C.framed_capture(dev.dev, dev.T, cfg)
    s = r0.summary()
    sec['summary'] = s
    for k, v in s.items():
        rep.kv(f'summary.{k}', v)
    rep.kv('packet sample counts (first 5)', str(r0.packet_samples[:5]))
    if r0.iq is not None and len(r0.iq) > 1024:
        # Multi-packet reassembly check: a FixedPoints frame is one contiguous
        # acquisition, so the phase increment of the CW tone must be constant.
        # A torn join would show an outlier at every packet boundary.
        pkt = int(r0.info.get('packet_samples') or 0)
        ph = np.unwrap(np.angle(r0.iq))
        d = np.diff(ph)
        resid = d - np.median(d)
        rep.kv('phase-increment stdev rad', float(d.std()))
        if pkt and len(d) > pkt + 2:
            edges = np.arange(1, len(d) // pkt) * pkt - 1
            interior = np.setdiff1d(np.arange(1, len(d) - 1), edges)
            rep.kv('packet-boundary |residual| max rad',
                   float(np.abs(resid[edges]).max()))
            rep.kv('interior |residual| max rad',
                   float(np.abs(resid[interior]).max()))


def cmd_no_trigger(rep: Report, dev, cfg: C.CaptureConfig) -> None:
    sec = rep.section('FixedPoints without BusTriggerStart (timeout behaviour)')
    import copy
    c = copy.copy(cfg)
    c.bus_timeout_ms = 500
    t0 = time.monotonic()
    r = C.framed_capture(dev.dev, dev.T, c, pre_trigger=False)
    dt = time.monotonic() - t0
    sec['status'] = r.first_status
    sec['elapsed_s'] = round(dt, 3)
    rep.kv('status without trigger', r.first_status)
    rep.kv('elapsed s (bus timeout 500 ms)', round(dt, 3))
    rep.kv('-10 == BusTimeOut', r.first_status == -10)


def cmd_depth(rep: Report, dev, decimates: list[int]) -> None:
    sec = rep.section('reachable FixedPoints depth')
    lengths = [2 ** k for k in (14, 16, 18, 20, 22, 24)]
    rows = []
    for dec in decimates:
        for n in lengths:
            cfg = C.CaptureConfig(center_hz=CENTER, decimate=dec,
                                  trigger_mode='fixed_points', trigger_length=n,
                                  bus_timeout_ms=5000)
            t0 = time.monotonic()
            r = C.framed_capture(dev.dev, dev.T, cfg)
            dt = time.monotonic() - t0
            got = 0 if r.iq is None else len(r.iq)
            rows.append([dec, n, r.first_status, got, r.packets_ok,
                         r.packets_err, round(dt, 3)])
    rep.table(['decimate', 'req samples', 'status', 'got', 'pkts ok', 'pkts err',
               'elapsed s'], rows, sec)
    for dec in decimates:
        ok = [r[1] for r in rows if r[0] == dec and r[2] == 0 and r[3] == r[1]]
        deep = max(ok) if ok else 0
        rep.kv(f'max complete depth dec={dec}', deep)
        rep.kv(f'max depth seconds dec={dec}', round(deep / (62.5e6 / dec), 4))


def cmd_stream(rep: Report, dev, decimates: list[int], seconds: float) -> None:
    sec = rep.section('Adaptive continuous stream')
    rows = []
    for dec in decimates:
        cfg = C.CaptureConfig(center_hz=CENTER, decimate=dec,
                              trigger_mode='adaptive', bus_timeout_ms=2000)
        r = C.stream_capture(dev.dev, dev.T, cfg, seconds=seconds)
        s = r.summary()
        rows.append([dec, round(s['fs'] / 1e6, 4), round(s['effective_rate'] / 1e6, 4),
                     round(s['loss_ppm'], 1), round(s['usb_mbytes_s'], 2),
                     s['packets_ok'], s['packets_err'], s['gap_max_ms'],
                     s['gap_p99_ms']])
    rep.table(['dec', 'fs MSPS', 'eff MSPS', 'loss ppm', 'MB/s', 'pkts ok',
               'pkts err', 'gap max ms', 'gap p99 ms'], rows, sec)


def cmd_soak(rep: Report, dev, args) -> None:
    """Sustained Adaptive run: per-window loss, packet errors and CPU share."""
    sec = rep.section(f'Adaptive soak ({args.soak_seconds:g} s per decimate)')
    rows = []
    for dec in [int(d) for d in args.decimates.split(',')]:
        cfg = C.CaptureConfig(center_hz=CENTER, decimate=dec,
                              trigger_mode='adaptive', bus_timeout_ms=2000)
        st, out, info = C.configure(dev.dev, dev.T, cfg)
        if st != 0:
            rows.append([dec, float('nan'), 0, 0, 0, float('nan')])
            continue
        fs = float(info.IQSampleRate)
        stream = C.new_stream()
        dev.T.dll.IQS_BusTriggerStart(dev.T.pointer(dev.dev))
        discarded = C.drain(dev.dev, dev.T, stream, 0.25)
        t0 = time.monotonic()
        cpu0 = time.process_time()
        win_samples = 0
        wins, errs_total, ok_total = [], 0, 0
        statuses: dict[int, int] = {}
        nxt = t0 + 10.0
        try:
            while time.monotonic() - t0 < args.soak_seconds:
                status, raw = C.packet(dev.dev, dev.T, stream)
                if status != 0:
                    errs_total += 1
                    statuses[status] = statuses.get(status, 0) + 1
                    continue
                ok_total += 1
                win_samples += len(raw) // 2
                now = time.monotonic()
                if now >= nxt:
                    wins.append(win_samples / (now - (nxt - 10.0)) / fs)
                    win_samples = 0
                    nxt += 10.0
        finally:
            dev.T.dll.IQS_BusTriggerStop(dev.T.pointer(dev.dev))
        wall = time.monotonic() - t0
        cpu = time.process_time() - cpu0
        rate = (ok_total * int(info.PacketSamples)) / wall
        rows.append([dec, round(rate / 1e6, 4), ok_total, errs_total,
                     round(100 * (1 - rate / fs), 4), round(100 * cpu / wall, 1)])
        rep.kv(f'dec {dec} settle-drained packets', discarded)
        rep.kv(f'dec {dec} error statuses', str(statuses))
        rep.kv(f'dec {dec} window rate ratios',
               ' '.join(f'{w:.5f}' for w in wins))
    rep.table(['dec', 'eff MSPS', 'pkts ok', 'pkts err', 'rate deficit %', 'CPU %'],
              rows, sec)
    rep.kv('note', 'CPU % is this process (IQS fetch + NumPy copy) over the run wall time')


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('cmd', choices=('framed', 'no-trigger', 'depth', 'stream', 'soak',
                                    'all'))
    ap.add_argument('--center', type=float, default=CENTER)
    ap.add_argument('--decimate', type=int, default=8)
    ap.add_argument('--decimates', default='4,16,64')
    ap.add_argument('--trigger-length', type=int, default=262144)
    ap.add_argument('--repeat', type=int, default=5)
    ap.add_argument('--seconds', type=float, default=5.0)
    ap.add_argument('--soak-seconds', type=float, default=60.0)
    ap.add_argument('--tinysa-port', default=None)
    ap.add_argument('--no-tinysa', action='store_true')
    ap.add_argument('--json', default=None)
    args = ap.parse_args()
    if args.json is None:
        args.json = os.path.join(os.path.dirname(__file__),
                                 f'iq_capture_{args.cmd}.json')

    sa = None
    with exclusive_websa():
        if not args.no_tinysa:
            sa = tiny_sa(args.tinysa_port)
            sa.cw(TONE_FREQ, TONE_DBM)
            time.sleep(0.4)
        try:
            with DeviceSession() as dev:
                rep = Report(f'IQS capture capability ({args.cmd})')
                rep.data['device'] = dev.info()
                rep.data['hardware'] = dev.hardware_state()
                rep.kv('device uid', dev.info()['uid'])
                rep.kv('model', dev.info()['model'])
                rep.kv('firmware mfw/ffw',
                       f"{dev.info()['mfw']}/{dev.info()['ffw']}")
                rep.kv('bus Mbps', dev.info()['bus_mbps'])
                rep.kv('SignalSourceEn', dev.hardware_state()['SignalSourceEn'])
                base = C.CaptureConfig(center_hz=args.center, decimate=args.decimate,
                                       trigger_mode='fixed_points',
                                       trigger_length=args.trigger_length)
                _conditions(rep, base)
                if args.cmd in ('framed', 'all'):
                    cmd_framed(rep, dev, args.repeat, base)
                if args.cmd in ('no-trigger', 'all'):
                    cmd_no_trigger(rep, dev, base)
                if args.cmd in ('depth', 'all'):
                    cmd_depth(rep, dev, [int(d) for d in args.decimates.split(',')])
                if args.cmd in ('stream', 'all'):
                    cmd_stream(rep, dev, [int(d) for d in args.decimates.split(',')],
                               args.seconds)
                if args.cmd in ('soak', 'all'):
                    cmd_soak(rep, dev, args)
                rep.save(args.json)
        finally:
            if sa is not None:
                sa.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
