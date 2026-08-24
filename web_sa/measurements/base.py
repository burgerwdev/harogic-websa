"""
measurements/base.py —— 测量会话基类 + 标准扫频会话

会话对象化: std/harmonic/pnm 统一接口 start/step/stop + enter/exit 配置快照。
来源: web_sa/server.py (v0.11.1) 的 set_mode/_snap_std/_restore_std 迁移。
"""
from __future__ import annotations

from dataclasses import dataclass


@dataclass
class ConfigSnapshot:
    """进入测量会话前的设备配置快照 (退出时恢复)。"""
    center: float
    span: float
    points: int
    rbw_mode: str
    rbw_hz: float
    vbw_mode: str
    vbw_hz: float
    ref: float
    atten: int
    preamp: int
    ifgain: int
    gain_strategy: int
    window: int
    spur: str


class MeasurementSession:
    """测量会话基类。name ∈ {std, harmonic, pnm}。"""

    name = 'std'

    def __init__(self, dev):
        self.dev = dev
        self.snapshot: ConfigSnapshot | None = None

    def enter(self) -> None:
        """进入会话: 快照标准配置。"""
        s = self.dev.state
        self.snapshot = ConfigSnapshot(
            center=s.center_hz, span=s.span_hz, points=s.points_req,
            rbw_mode=s.rbw_mode, rbw_hz=s.rbw_hz, vbw_mode=s.vbw_mode, vbw_hz=s.vbw_hz,
            ref=s.ref_level, atten=s.atten, preamp=s.preamplifier, ifgain=s.ifgain,
            gain_strategy=s.gain_strategy, window=s.window, spur=s.spur_mode)

    def exit(self) -> None:
        """退出会话: 恢复快照并重配设备。"""
        snap = self.snapshot
        if snap is None:
            return
        s = self.dev.state
        s.center_hz, s.span_hz = snap.center, snap.span
        s.points_req = snap.points
        s.rbw_mode, s.rbw_hz = snap.rbw_mode, snap.rbw_hz
        s.vbw_mode, s.vbw_hz = snap.vbw_mode, snap.vbw_hz
        s.ref_level = snap.ref
        s.atten, s.preamplifier = snap.atten, snap.preamp
        s.ifgain, s.gain_strategy = snap.ifgain, snap.gain_strategy
        s.window, s.spur_mode = snap.window, snap.spur
        self.snapshot = None
        self.dev.configure_swp()

    def step(self):
        """单步执行 (publisher 调用), 返回 (frames, json_msgs)。"""
        return [], []


class StdSession(MeasurementSession):
    """标准扫频会话: 连续 fetch_sweep 推 FREQ/POWR 帧。"""

    name = 'std'

    def step(self):
        r = self.dev.fetch_sweep()
        if r is None:
            return [], []
        f, p = r
        frames = []
        fv = self.dev.state.freq_version
        self.dev.last_freq = f          # 供新客户端连接时下发
        self.dev.last_freq_ver = fv
        from .framer import encode_freq, encode_powr
        frames.append(encode_freq(fv, f, self.dev.state.sweep_ms))
        frames.append(encode_powr(fv, p, self.dev.state.sweep_ms))
        return frames, []
