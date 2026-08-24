"""
measurements/harmonic.py —— 谐波测量会话 (官方风格: 全频段真实频谱 + 逐谐波自动调谐)

来源: web_sa/server.py (v0.11.1) harm_step/pub_harmonic 迁移。
"""
from __future__ import annotations

import numpy as np

from ..config import fit_span
from .base import MeasurementSession


class HarmonicSession(MeasurementSession):
    name = 'harmonic'

    def __init__(self, dev):
        super().__init__(dev)
        self.f0 = 1e9
        self.count = 5
        self.span = 10e6
        self._seq = None          # 当前逐谐波序列
        self._seq_n = 1
        self._base = None
        self._scan_phase = True   # 全频段扫描(推真实频谱帧) / 逐谐波测峰
        self._scan_cnt = 0

    # ---- 参数 (SET_HARM 入口) ----
    def set_params(self, f0=None, count=None, span=None):
        if f0 is not None:
            self.f0 = float(max(self.dev.state.caps.freq_min_hz,
                                min(self.dev.state.caps.freq_max_hz, f0)))
        if count is not None:
            self.count = int(max(1, min(10, count)))
        if span is not None:
            self.span = float(max(1.0, min(100e6, span)))
        self._seq = None          # 参数变更重启序列

    # ---- 全频段扫描 (推真实频谱帧) ----
    def _scan_step(self):
        s = self.dev.state
        hi = min(s.caps.freq_max_hz, self.f0 * (self.count + 1))
        lo = max(s.caps.freq_min_hz, self.f0 / 4)
        center = (lo + hi) / 2
        span = min(hi - lo, fit_span(center, hi - lo, s.caps))
        s.center_hz, s.span_hz = center, span
        self.dev.configure_swp()
        r = self.dev.fetch_sweep()
        frames = []
        if r is not None:
            from .framer import encode_freq, encode_powr
            f, p = r
            fv = s.freq_version
            frames.append(encode_freq(fv, f, s.sweep_ms))
            frames.append(encode_powr(fv, p, s.sweep_ms))
        self._scan_cnt += 1
        if self._scan_cnt >= 8:
            self._scan_cnt = 0
            self._scan_phase = False
        return frames, []

    # ---- 逐谐波精确测量 ----
    def _measure_step(self):
        if self._seq is None:
            self._seq, self._seq_n, self._base = [], 1, None
        n = self._seq_n
        f = self.f0 * n
        if n > self.count or f > self.dev.state.caps.freq_max_hz:
            res = list(self._seq)
            self.dev.state.harm_results = res
            self._seq = None
            self._scan_phase = True   # 回全频段刷新
            return [], [{'cmd': 'HARM', 'list': res, 'f0': self.f0}]
        s = self.dev.state
        center = max(s.caps.freq_min_hz, min(f, s.caps.freq_max_hz))
        s.center_hz, s.span_hz = center, min(self.span, fit_span(center, self.span, s.caps))
        self.dev.configure_swp()
        r = None
        for _try in range(6):       # 配置后立即扫频可能失败, 重试
            r = self.dev.fetch_sweep()
            if r is not None:
                break
            import time
            time.sleep(0.03)
        if r is not None:
            fr, pows = r
            if len(pows):
                mi = int(np.argmax(pows))
                amp = float(pows[mi]); ff = float(fr[mi])
                if self._base is None:
                    self._base = amp
                self._seq.append(dict(n=n, f=round(ff, 1), amp=round(amp, 2),
                                      dbc=round(amp - self._base, 2)))
        self._seq_n += 1
        return [], []

    def step(self):
        if self._scan_phase:
            return self._scan_step()
        return self._measure_step()
