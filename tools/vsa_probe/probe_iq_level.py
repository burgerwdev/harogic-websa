#!/usr/bin/env python3
"""Hardware probe: IQ amplitude/power calibration and DC/image behaviour.

``level``
    Absolute level of the IQS path. For several ``IQS_Profile.RefLevel_dBm``
    settings the tinySA level is stepped and the captured block is measured two
    ways: the whole-block mean power and the coherent CW tone power. This gives
    both the absolute agreement with the source and the input headroom (where
    the IQ path starts compressing).

``swept``
    The same tone measured by the vendor's own swept path through production
    code (``web_sa.hardware.device.HarogicDevice``), so the IQ numbers can be
    compared with the calibrated spectrum the UI already shows. The standard
    display configuration is used (span 10 MHz, RBW auto, 1000 points): with a
    200 kHz span and a minimum sweep time the vendor trace is not amplitude
    calibrated, which produced nonsense during development.

``dcimage``
    The IQS centre is not a neutral place: the DC canceller attenuates whatever
    sits at DC and a zero-IF downconverter puts an image at -f for every tone at
    +f. Measures the DCC profile by moving the IQS centre under a fixed tone,
    and the image rejection from one capture.

Usage (WebSA stopped automatically; restored afterwards if it was running):
    python3 tools/vsa_probe/probe_iq_level.py level --ref-levels 0,-30
    python3 tools/vsa_probe/probe_iq_level.py dcimage
    python3 tools/vsa_probe/probe_iq_level.py all
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
TONE_FREQ = CENTER + 200e3
TONE_DBM = -25.0
#: keep the tone 200 kHz above the IQS centre: away from the DC canceller notch
LEVEL_OFFSET = 200e3
#: standard WebSA display configuration for the vendor reference trace
SWEPT_SPAN = 10e6


def _iq_capture(dev, offset_hz: float, decimate: int, length: int, ref_dbm: float,
                dcc: str = 'high_pass', qdc: str = 'off') -> C.FramedCapture:
    cfg = C.CaptureConfig(center_hz=TONE_FREQ - offset_hz, decimate=decimate,
                          trigger_mode='fixed_points', trigger_length=length,
                          ref_level_dbm=ref_dbm, dcc=dcc, qdc=qdc)
    return C.framed_capture(dev.dev, dev.T, cfg)


def cmd_level(rep: Report, dev, levels: list[float], ref_levels: list[float],
              args) -> None:
    sec = rep.section('IQ path level vs source level and IQS RefLevel')
    rows = []
    for ref in ref_levels:
        for lvl in levels:
            args._sa.cw(TONE_FREQ, lvl)
            time.sleep(0.35)
            r = _iq_capture(dev, LEVEL_OFFSET, args.decimate, args.trigger_length, ref)
            if not len(r.iq):
                rows.append([ref, lvl, float('nan'), float('nan'), float('nan')])
                continue
            _, tone = A.tone_dbm(r.iq, r.fs, LEVEL_OFFSET, search_hz=50e3)
            rows.append([ref, lvl, A.mean_dbm(r.iq), tone, r.scale_to_v])
    rep.table(['IQS RefLevel', 'tinySA dBm', 'IQ mean dBm', 'IQ tone dBm',
               'ScaleToV'], rows, sec)
    a = np.array(rows, dtype=float)
    for ref in ref_levels:
        pts = sorted((float(r[1]), float(r[2])) for r in a
                     if r[0] == ref and np.isfinite(r[2]))
        lin = []
        for src, mean in pts:
            if abs(mean - src) > 1.5:      # first point outside 1.5 dB ends the linear range
                break
            lin.append((src, mean))
        if len(lin) >= 2:
            src = np.array([p[0] for p in lin])
            mean = np.array([p[1] for p in lin])
            rep.kv(f'RefLevel {ref:g}: slope mean-IQ vs source (dB/dB)',
                   float(np.polyfit(src, mean, 1)[0]))
            rep.kv(f'RefLevel {ref:g}: offset mean-IQ - source (dB)',
                   float((mean - src).mean()))
            rep.kv(f'RefLevel {ref:g}: linear up to source (dBm)', float(src.max()))
        if pts:
            rep.kv(f'RefLevel {ref:g}: worst compression seen (dB)',
                   float(max(m - s for s, m in pts if m - s < -1.5) if
                           any(m - s < -1.5 for s, m in pts) else 0.0))
    args._iq_rows = rows


def cmd_swept(rep: Report, levels: list[float], args) -> None:
    """Vendor-calibrated cross-check through the production swept path."""
    from web_sa.hardware.device import HarogicDevice
    from web_sa.measurements import make_session

    sec = rep.section('vendor swept path on the same tone (cross-check)')
    dev = HarogicDevice()
    ok, err = dev.open()
    if not ok:
        rep.kv('open', f'FAILED {err}')
        return
    try:
        s = dev.state
        s.center_hz = TONE_FREQ
        s.span_hz = SWEPT_SPAN
        s.ref_mode = 'manual'
        s.ref_level = 0.0
        s.rbw_mode = 'auto'
        s.vbw_mode = 'equal'
        s.points_req = 1000
        s.spur_mode = 'bypass'
        s.detector = 'pos_peak'
        s.sweep_time_mode = 0
        s.ifagc = 0
        make_session(dev, 'std')
        ok, err = dev.configure_swp()
        actual = dict(dev.state.actual)
        rep.kv('configure_swp', f'{ok} {err}')
        rep.kv('actual rbw Hz', actual.get('rbw'))
        rep.kv('actual points', actual.get('points'))
        rep.kv('actual span Hz', actual.get('span'))
        rows = []
        for lvl in levels:
            args._sa.cw(TONE_FREQ, lvl)
            time.sleep(1.0)
            peaks = []
            for _ in range(12):
                got = dev.fetch_sweep()
                if got is None:
                    continue
                freq, power = got
                i = int(np.nanargmax(power))
                peaks.append((float(freq[i]), float(power[i])))
                time.sleep(0.05)
            if peaks:
                p = np.array(peaks)
                rows.append([lvl, round(float(p[:, 1].mean()), 3),
                             round(float(p[:, 1].std()), 3),
                             round(float(p[:, 0].mean()), 1)])
        rep.table(['tinySA dBm', 'swept peak mean dBm', 'stdev dB', 'peak Hz'],
                  rows, sec)
        args._swept_rows = rows
    finally:
        dev.close()


def compare(rep: Report, args) -> None:
    sw = {r[0]: r for r in getattr(args, '_swept_rows', [])}
    if not sw:
        return
    iq = {}
    for ref, lvl, mean, tone, _ in getattr(args, '_iq_rows', []):
        if np.isfinite(mean):
            iq.setdefault(lvl, []).append((ref, mean, tone))
    rows = []
    for lvl in sorted(set(iq) & set(sw)):
        for ref, mean, tone in iq[lvl]:
            rows.append([lvl, ref, sw[lvl][1], mean, tone, mean - sw[lvl][1],
                         tone - sw[lvl][1]])
    if not rows:
        return
    sec = rep.section('IQ vs vendor swept agreement (delta in dB)')
    rep.table(['tinySA', 'RefLevel', 'swept dBm', 'IQ mean dBm', 'IQ tone dBm',
               'mean-swept', 'tone-swept'], rows, sec)
    a = np.array(rows, dtype=float)
    rep.kv('mean (IQ mean - swept) dB', float(a[:, 5].mean()))
    rep.kv('stdev (IQ mean - swept) dB', float(a[:, 5].std()))
    rep.kv('mean (IQ tone - swept) dB', float(a[:, 6].mean()))
    rep.kv('stdev (IQ tone - swept) dB', float(a[:, 6].std()))
    rep.kv('swept peak stdev range (dB)',
           f'{min(r[2] for r in getattr(args, "_swept_rows", [])):.2f} .. '
           f'{max(r[2] for r in getattr(args, "_swept_rows", [])):.2f}')
    rep.kv('note', 'the vendor swept peak is the bin-aligned trace peak; its own '
                   'spread (see stdev column) bounds this cross-check to ~2 dB')


def cmd_dcimage(rep: Report, dev, args) -> None:
    offsets = [float(x) for x in args.offsets.split(',')]
    sec = rep.section('DC canceller profile vs tone offset from the IQS centre')
    rows = []
    for off in offsets:
        for dcc in args.dcc.split(','):
            r = _iq_capture(dev, off, args.decimate, args.trigger_length,
                            args.ref_level, dcc=dcc)
            if not len(r.iq):
                rows.append([off, dcc, float('nan'), float('nan')])
                continue
            # the IQS centre moves, so the tone is at +offset relative to DC
            _, tone = A.tone_dbm(r.iq, r.fs, off,
                                 search_hz=max(200.0, abs(off) * 0.05))
            rows.append([off, dcc, A.mean_dbm(r.iq), tone])
    rep.table(['offset Hz', 'dcc', 'mean dBm', 'tone dBm'], rows, sec)
    for dcc in args.dcc.split(','):
        sub = [r for r in rows if r[1] == dcc and np.isfinite(r[3])]
        if sub:
            ref = max(r[3] for r in sub)
            rep.kv(f'tone level span {dcc} (dB)', float(ref - min(r[3] for r in sub)))

    sec2 = rep.section('image rejection (spur at -f for a tone at +f)')
    rows2 = []
    for off in [float(x) for x in args.image_offsets.split(',')]:
        for dcc in args.dcc.split(','):
            r = _iq_capture(dev, off, args.decimate, args.trigger_length,
                            args.ref_level, dcc=dcc)
            if not len(r.iq):
                continue
            # the image of a tone at +offset sits at -offset, inside the passband
            _, want = A.tone_dbm(r.iq, r.fs, off,
                                 search_hz=max(200.0, abs(off) * 0.05))
            _, image = A.tone_dbm(r.iq, r.fs, -off,
                                  search_hz=max(200.0, abs(off) * 0.05))
            rows2.append([off, dcc, want, image, want - image])
    rep.table(['offset Hz', 'dcc', 'tone dBm', 'image dBm', 'image rejection dB'],
              rows2, sec2)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('cmd', choices=('level', 'dcimage', 'all'))
    ap.add_argument('--levels', default='-30,-25,-20,-15')
    ap.add_argument('--ref-levels', default='0,-30')
    ap.add_argument('--ref-level', type=float, default=0.0)
    ap.add_argument('--offsets', default='0,500,1000,3000,10000,30000,100000')
    # decimate 16 -> 3.906 MSPS, so the passband is +-1.95 MHz: keep the image test inside it
    ap.add_argument('--image-offsets', default='100000,300000,700000,1500000')
    ap.add_argument('--dcc', default='high_pass,off')
    ap.add_argument('--decimate', type=int, default=16)
    ap.add_argument('--trigger-length', type=int, default=262144)
    ap.add_argument('--tinysa-port', default=None)
    ap.add_argument('--json', default=None)
    args = ap.parse_args()
    if args.json is None:
        args.json = os.path.join(os.path.dirname(__file__),
                                 f'iq_level_{args.cmd}.json')

    levels = [float(x) for x in args.levels.split(',')]
    ref_levels = [float(x) for x in args.ref_levels.split(',')]
    rep = Report(f'IQ level and DC/image behaviour ({args.cmd})')
    with exclusive_websa():
        sa = tiny_sa(args.tinysa_port)
        args._sa = sa
        try:
            sa.cw(TONE_FREQ, TONE_DBM)
            time.sleep(0.4)
            if args.cmd in ('level', 'all'):
                with DeviceSession() as dev:
                    rep.data['device'] = dev.info()
                    rep.kv('device uid', dev.info()['uid'])
                    rep.kv('tone_freq_hz', TONE_FREQ)
                    rep.kv('decimate', args.decimate)
                    rep.kv('trigger_length', args.trigger_length)
                    cmd_level(rep, dev, levels, ref_levels, args)
                cmd_swept(rep, levels, args)
                compare(rep, args)
            if args.cmd in ('dcimage', 'all'):
                with DeviceSession() as dev:
                    rep.kv('tone_freq_hz', TONE_FREQ)
                    rep.kv('decimate', args.decimate)
                    rep.kv('trigger_length', args.trigger_length)
                    rep.kv('RefLevel dBm', args.ref_level)
                    cmd_dcimage(rep, dev, args)
            rep.save(args.json)
        finally:
            sa.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
