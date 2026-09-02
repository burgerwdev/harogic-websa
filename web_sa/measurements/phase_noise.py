"""
measurements/phase_noise.py -- phase-noise (PNM) measurement session

Source: migrated from _pnm_start/pnm_step of web_sa/server.py (v0.11.1).
Features: each Get pushes partial results in real time; parameter changes (dirty) are
reconfigured after the current cycle completes.
"""
from __future__ import annotations

from ..hardware import sdk_bindings as sb
from .base import MeasurementSession


class PhaseNoiseSession(MeasurementSession):
    name = 'pnm'

    def __init__(self, dev):
        super().__init__(dev)
        self.center = 1e9
        self.threshold = -50.0
        self.rbwratio = 0.02
        self.start_offset = 100.0
        self.stop_offset = 10e6
        self.traceavg = 4
        self.dirty = False
        self._ready = False
        self._started = False
        self._i = 0
        self._stall = 0
        self._info = None
        self._cf = sb.c_double(); self._cp = sb.c_float(); self._rf = sb.c_float()
        self._freq = None
        self._pn = None

    def set_params(self, center=None, threshold=None, traceavg=None,
                   start=None, stop=None):
        if center is not None:
            self.center = float(max(self.dev.state.caps.freq_min_hz,
                                    min(self.dev.state.caps.freq_max_hz, center)))
        if threshold is not None:
            self.threshold = float(threshold)
        if traceavg is not None:
            self.traceavg = int(max(1, min(1000, traceavg)))
        if start is not None:
            self.start_offset = float(max(1.0, min(9e6, start)))
        if stop is not None:
            self.stop_offset = float(max(10.0, min(1e7, stop)))
        self.dirty = True          # reconfigure after the current cycle completes (do not interrupt)

    def enter(self):
        super().enter()
        self._configure()

    def exit(self):
        with self.dev._hw:
            try:
                if self._ready:
                    sb.dll.PNM_StopMeasure(sb.pointer(self.dev.dev))
            except Exception:
                pass
        self._ready = False
        self._started = False
        super().exit()

    def _configure(self):
        with self.dev._hw:
            self._configure_locked()

    def _configure_locked(self):
        d = self.dev.dev
        prof = sb.PNM_Profile_TypeDef()
        sb.dll.PNM_ProfileDeInit(sb.pointer(d), sb.pointer(prof))
        prof.CenterFreq = self.center
        prof.Threshold = self.threshold
        prof.RBWRatio = self.rbwratio
        prof.StartOffsetFreq = self.start_offset
        prof.StopOffsetFreq = self.stop_offset
        prof.TraceAverage = self.traceavg
        out = sb.PNM_Profile_TypeDef()
        info = sb.PNM_MeasInfo_TypeDef()
        st = sb.dll.PNM_Configuration(sb.pointer(d), sb.pointer(prof),
                                      sb.pointer(out), sb.pointer(info))
        if st != 0:
            raise RuntimeError('PNM_Configuration %d' % st)
        self._info = info
        n = max(int(info.TracePoints), 1)
        self._freq = (sb.c_double * n)()
        self._pn = (sb.c_float * n)()
        self._ready = True
        self._started = False
        self._i = 0
        self._stall = 0
        self.dirty = False

    def step(self):
        if not self._ready or self._info is None:
            return [], []
        with self.dev._hw:
            if not self._started and self.dirty:
                try:
                    self._configure()
                except Exception:
                    self.dirty = False
            d = self.dev.dev
            info = self._info
            if not self._started:
                st = sb.dll.PNM_StartMeasure(sb.pointer(d))
                if st != 0:
                    return [], []
                self._started = True
                self._i = 0
                self._stall = 0
            n = max(int(info.TracePoints), 1)
            upd = (sb.c_uint32 * max(int(info.Segments), 1))()
            aux = sb.PNM_AuxInfo_TypeDef()
            st = sb.dll.PNM_GetPartialUpdatedFullTrace(
                sb.pointer(d), sb.pointer(self._cf), sb.pointer(self._cp),
                self._freq, self._pn, upd, sb.pointer(aux), sb.pointer(self._rf))
            if st == 0:
                self._i += 1
                self._stall = 0
            else:
                self._stall += 1
                if self._stall > info.PartialUpdateCounts + 8:
                    self._started = False
                    return [], []
                return [], []
            res = dict(carrier_freq=float(self._cf.value),
                       carrier_power=float(self._cp.value),
                       offset=list(self._freq[:n]), pn=list(self._pn[:n]),
                       ref=float(self._rf.value), traceavg=float(self.traceavg))
            if self._i >= info.PartialUpdateCounts:
                self._started = False
                self.dirty = False
                res['done'] = True
                self.dev.state.pnm_last = res
                return [], [{'cmd': 'PNM', **res}]
            res['done'] = False
            res['progress'] = round(self._i / max(info.PartialUpdateCounts, 1) * 100)
            return [], [{'cmd': 'PNM', **res}]
