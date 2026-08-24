"""
hardware/device.py —— 设备抽象 (SAN 全系列)

来源: web_sa/server.py (v0.11.1) HarogicDevice 迁移; 行为保持, 结构重构。
- 业务层唯一设备入口: open/configure/fetch/query
- 所有 SDK 调用经 hardware/sdk_bindings 串行执行 (事件循环内)
"""
from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from ..config import DeviceCapabilities
from . import sdk_bindings as sb

# 硬件枚举便捷别名
SWP = sb
T = sb  # 类型别名


class DeviceError(RuntimeError):
    pass


@dataclass
class DeviceState:
    """设备只读状态 (序列化到 WS STATUS)。"""
    connected: bool = False
    label: str = ''
    detail: str = ''
    device_detail: dict = field(default_factory=dict)
    caps: DeviceCapabilities = None          # 型号能力
    center_hz: float = 1e9
    span_hz: float = 100e6
    ref_level: float = 0.0
    rbw_mode: str = 'manual'
    rbw_hz: float = 100e3
    vbw_mode: str = 'manual'
    vbw_hz: float = 100e3
    points_req: int = 1000
    window: int = 1
    spur_mode: str = 'bypass'
    atten: int = -1
    preamplifier: int = 0
    ifgain: int = 2
    gain_strategy: int = 0
    amp_atten: int = -1
    preamplifier_actual: int | None = None
    ref_clock: str = 'internal'
    has_docxo: bool = False
    mode: str = 'std'
    pnm_supported: bool = False
    actual: dict = field(default_factory=dict)
    gnss: dict = field(default_factory=dict)
    sweep_ms: float = 0.0
    freq_version: int = 0
    config_version: int = 0
    last_error: str = ''
    refclk_ppm: float = 0.0
    calibrating: bool = False
    last_cal_freq: float = 0.0
    refclk_out: bool = False   # 参考时钟输出使能
    # 测量结果
    harm_results: list = field(default_factory=list)
    pnm_last: dict | None = None


class HarogicDevice:
    """设备封装: 生命周期 + 扫频 + 查询 + 测量会话宿主。"""

    def __init__(self):
        self.dev = sb.c_void_p()
        self.dsp = sb.c_void_p()
        self.state = DeviceState()
        self.session = None
        # 扫频缓冲区(尺寸变化时重建,避免每帧分配) —— 参考原实现
        self._freq_buf = None
        self._spec_buf = None
        self._ifreq_buf = None
        self._ispec_buf = None
        self._cnt = sb.c_uint32(0)
        self._meas_aux = sb.Full_MeasAuxInfo()   # 完整结构(含 RefClkFreqOffset ppm)
        self._trace_points = 0
        self._user_start = 0.0
        self._user_stop = 0.0
        self.preset_defaults = None   # 启动时读取的 SWP 默认配置缓存
        self.last_freq = None      # 最近扫频频率轴(新 WS 客户端连接时下发)
        self.last_freq_ver = 0
        self._sweep_ema = None

    # ---------------- 生命周期 ----------------
    def open(self) -> tuple[bool, str]:
        bp = sb.BootProfile_TypeDef()
        bi = sb.BootInfo_TypeDef()
        bp.DevicePowerSupply = sb.htra_api.DevicePowerSupply_TypeDef.USBPortAndPowerPort
        bp.PhysicalInterface = sb.htra_api.PhysicalInterface_TypeDef.USB
        st = sb.dll.Device_Open(sb.pointer(self.dev), sb.c_int(0), sb.pointer(bp), sb.pointer(bi))
        if st != 0 and st != -49:
            self.state.last_error = 'Device_Open status=%d' % st
            return False, self.state.last_error
        sb.dll.DSP_Open(sb.pointer(self.dsp))
        self.state.pnm_supported = sb.PNM_SUPPORTED
        di = bi.DeviceInfo
        self.state.label = 'UID:%012X Model:%d HW:%d MFW:%d FFW:%d' % (
            di.DeviceUID, di.Model, di.HardwareVersion, di.MFWVersion, di.FFWVersion)
        self.state.caps = DeviceCapabilities.from_model(di.Model, sb.PNM_SUPPORTED)
        self.state.device_detail = dict(
            uid='%012X' % di.DeviceUID, model=di.Model, hw=di.HardwareVersion,
            mfw=di.MFWVersion, ffw=di.FFWVersion,
            bus_speed=bi.BusSpeed, bus_ver=bi.BusVersion,
            api_ver=bi.APIVersion, warnings=bi.Warnings, errors=bi.Errors)
        self.state.connected = True
        self.configure_swp()
        self._detect_docxo()
        self.load_preset_defaults()   # 读取设备默认配置供 preset 使用
        # DOCXO 探测用临时配置会改变设备实际参数(如 TracePoints),
        # 必须重新下发标准配置并刷新缓冲,否则 GetFullSweep 越界写
        self.configure_swp()
        return True, 'ok'

    def close(self) -> None:
        try:
            sb.dll.Device_Close(sb.pointer(self.dev))
        except Exception:
            pass
        self.state.connected = False

    # ---------------- 配置 (参考原 web_sa/server.py 实现) ----------------
    def load_preset_defaults(self) -> None:
        """启动时读取一次 SWP_ProfileDeInit 设备默认配置并缓存(不写死, 支持不同 SAN 型号)。
        preset 调用时用缓存值恢复。"""
        try:
            p = sb.SWP_Profile_TypeDef()
            sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(p))
            self.preset_defaults = dict(
                center=float(p.CenterFreq_Hz), span=float(p.Span_Hz),
                ref=float(p.RefLevel_dBm), rbw=float(p.RBW_Hz),
                points=int(p.TracePoints), atten=int(p.Atten),
                window=int(p.Window.value if hasattr(p.Window, 'value') else p.Window),
                rbw_mode='auto' if int(getattr(p.RBWMode, 'value', p.RBWMode)) else 'manual')
        except Exception:
            self.preset_defaults = None

    def apply_preset(self) -> dict:
        """应用缓存的设备默认配置, 返回默认值(供 STATUS/前端)。"""
        d = self.preset_defaults
        if not d:
            self.load_preset_defaults()
            d = self.preset_defaults
        if not d:
            return {}
        s = self.state
        s.center_hz, s.span_hz = d['center'], d['span']
        s.ref_level = d['ref']
        s.rbw_hz = d['rbw']
        s.points_req = d['points']
        s.atten = d['atten']
        s.window = d['window']
        s.rbw_mode = d['rbw_mode']
        self.configure_swp()
        return d

    def _profile(self):
        T = sb
        s = self.state
        p = T.SWP_Profile_TypeDef()
        sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(p))
        start = max(s.caps.freq_min_hz, s.center_hz - s.span_hz / 2)
        stop = min(s.caps.freq_max_hz, s.center_hz + s.span_hz / 2)
        if stop <= start:
            stop = min(s.caps.freq_max_hz, start + 1e3)
        p.FreqAssignment = T.SWP_FreqAssignment_TypeDef.StartStop
        p.StartFreq_Hz = start
        p.StopFreq_Hz = stop
        p.RefLevel_dBm = max(-50, min(30, s.ref_level))
        if s.rbw_mode == 'auto':
            p.RBWMode = T.RBWMode_TypeDef.RBW_Auto
        else:
            p.RBWMode = T.RBWMode_TypeDef.RBW_Manual
            p.RBW_Hz = max(100.0, min(10e6, s.rbw_hz))
        p.VBWMode = {'equal': T.VBWMode_TypeDef.VBW_EqualToRBW,
                     'tenth': T.VBWMode_TypeDef.VBW_TenPercentRBW,
                     'bypass': T.VBWMode_TypeDef.VBW_TenTimesRBW,
                     'manual': T.VBWMode_TypeDef.VBW_Manual}.get(
                         s.vbw_mode, T.VBWMode_TypeDef.VBW_TenTimesRBW)
        p.VBW_Hz = max(10.0, min(10e6, s.vbw_hz))
        p.Window = T.Window_TypeDef(int(s.window))
        p.Atten = int(s.atten)
        p.Preamplifier = T.PreamplifierState_TypeDef.AutoOn if s.preamplifier == 0 \
            else T.PreamplifierState_TypeDef.ForcedOff
        p.IFGainGrade = int(s.ifgain)
        p.GainStrategy = T.GainStrategy_TypeDef.LowNoisePreferred if s.gain_strategy == 0 \
            else T.GainStrategy_TypeDef.HighLinearityPreferred
        rc_map = {'internal': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal,
                  'external': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External,
                  'premium': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal_Premium,
                  'external_forced': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External_Forced}
        p.ReferenceClockSource = rc_map.get(s.ref_clock, T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal)
        # 外部参考频率: 用户外部信号源 10MHz(官方 SCIPI 示例 ROSC:EXT:FREQ 10MHz)
        # 注意: SystemClockSource 切外部是危险配置(厂商指导下使用), 不设置, 避免设备挂死
        p.ExternalSystemClockFrequency = 10e6
        p.EnableReferenceClockOut = 1 if s.refclk_out else 0
        p.SweepTimeMode = T.SweepTimeMode_TypeDef.SWTMode_minSWT
        p.TracePoints = int(max(51, min(4000, s.points_req)))
        p.TracePointsStrategy = T.TracePointsStrategy_TypeDef.SweepSpeedPreferred
        p.TraceAlign = T.TraceAlign_TypeDef.AlignToStart
        p.SpurRejection = T.SpurRejection_TypeDef.Standard if s.spur_mode == 'standard' else (
            T.SpurRejection_TypeDef.Enhanced if s.spur_mode == 'enhanced'
            else T.SpurRejection_TypeDef.Bypass)
        return p

    def configure_swp(self):
        if not self.state.connected:
            return False, 'not connected'
        pin = self._profile()
        pout = sb.SWP_Profile_TypeDef()
        ti = sb.SWP_TraceInfo_TypeDef()
        st = sb.dll.SWP_Configuration(sb.pointer(self.dev), sb.pointer(pin),
                                      sb.pointer(pout), sb.pointer(ti))
        if st != 0:
            self.state.last_error = 'SWP_Configuration status=%d' % st
            return False, self.state.last_error
        self.state.actual = dict(center=pout.CenterFreq_Hz, span=pout.Span_Hz,
                                 start=pout.StartFreq_Hz, stop=pout.StopFreq_Hz,
                                 ref=pout.RefLevel_dBm, rbw=pout.RBW_Hz, vbw=pout.VBW_Hz,
                                 points=ti.FullsweepTracePoints, est_min=ti.EstimateMinSweepTime,
                                 refclk=pout.ReferenceClockFrequency,
                                 refclk_src=int(pout.ReferenceClockSource.value))
        self._trace_points = int(ti.FullsweepTracePoints)
        self._user_start = float(pout.StartFreq_Hz)
        self._user_stop = float(pout.StopFreq_Hz)
        self.state.config_version += 1
        self.state.freq_version += 1
        self._read_amp_atten()
        return True, 'ok'

    def _read_amp_atten(self) -> None:
        """回读实际衰减/前置状态 (参考原实现 Device_GetAmpAttenState)。"""
        try:
            amp = sb.PreamplifierState_TypeDef()
            att = sb.c_int(0)
            sp = sb.c_uint8(0)
            sb.dll.Device_GetAmpAttenState(sb.pointer(self.dev), sb.pointer(amp),
                                           sb.pointer(att), sb.pointer(sp))
            self.state.amp_atten = att.value
            self.state.preamplifier_actual = int(amp.value)
        except Exception:
            pass

    # ---------------- 扫频 ----------------
    def fetch_sweep(self):
        """返回 (freq_np_float64, power_np_float32) 或 None。事件循环内串行调用。"""
        n = getattr(self, '_trace_points', 0)
        if n <= 0 or not self.state.connected:
            return None
        try:
            if self._freq_buf is None or len(self._freq_buf) != n:
                self._freq_buf = (sb.c_double * n)()
                self._spec_buf = (sb.c_float * n)()
                self._ifreq_buf = (sb.c_double * n)()
                self._ispec_buf = (sb.c_float * n)()
            st = sb.dll.SWP_GetFullSweep(sb.pointer(self.dev), self._freq_buf,
                                         self._spec_buf, sb.pointer(self._meas_aux))
            if st != 0:
                return None
            self.state.refclk_ppm = float(getattr(self._meas_aux, 'RefClkFreqOffset', 0.0))
            sb.dll.DSP_InterceptSpectrum(
                sb.c_double(self._user_start), sb.c_double(self._user_stop),
                self._freq_buf, self._spec_buf, sb.c_uint32(n),
                self._ifreq_buf, self._ispec_buf, sb.pointer(self._cnt))
            c = self._cnt.value
            if c < 2:
                return None
            f = np.frombuffer(self._ifreq_buf, dtype=np.float64, count=c).copy()
            p = np.frombuffer(self._ispec_buf, dtype=np.float32, count=c).copy()
            mask = f >= 0.0
            if not mask.all():
                f = f[mask]; p = p[mask]
            if len(f) < 2:
                return None
            # 返回设备原生迹线(与 v0.5.3 一致):
            # 后端不重采样, 由前端 resampleTrace 处理点数变化;
            # 后端 np.interp 升采样会把窄信号拉成三角波
            return f, p
        except Exception as e:
            self.state.last_error = 'sweep: %r' % e
            return None

    # ---------------- 查询 ----------------
    def _detect_docxo(self) -> None:
        try:
            prof = T.SWP_Profile_TypeDef()
            po = T.SWP_Profile_TypeDef()
            ti = T.SWP_TraceInfo_TypeDef()
            sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(prof))
            prof.CenterFreq_Hz = self.state.center_hz
            prof.Span_Hz = self.state.span_hz
            prof.FreqAssignment = T.SWP_FreqAssignment_TypeDef.CenterSpan
            prof.ReferenceClockSource = T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal_Premium
            st = sb.dll.SWP_Configuration(sb.pointer(self.dev), sb.pointer(prof),
                                          sb.pointer(po), sb.pointer(ti))
            self.state.has_docxo = st == 0 and int(po.ReferenceClockSource) == 2
        except Exception:
            self.state.has_docxo = False

    def calibrate_ref_clock(self, trigger_count: int = 30) -> tuple[bool, float]:
        """GNSS 1PPS 校准内部参考时钟(阻塞, 调用方需后台线程 + 超时保护).
        GNSS 1PPS 不可用(无天线/未锁定)时 DLL 可能挂起等待, 需外部超时."""
        self.state.calibrating = True
        try:
            out = sb.c_double(0.0)
            st = sb.dll.Device_CalibrateRefClock(
                sb.pointer(self.dev), 1,  # CalibrateByGNSS1PPS
                1.0, sb.c_uint64(max(3, int(trigger_count))),
                sb.c_uint8(0), sb.pointer(out))
            if st != 0:
                return False, 0.0
            return True, float(out.value)
        except Exception:
            return False, 0.0
        finally:
            self.state.calibrating = False

    def query_gnss(self) -> dict:
        try:
            g = T.GNSSInfo_TypeDef()
            sb.dll.Device_GetGNSSInfo(sb.pointer(self.dev), sb.pointer(g))
            return dict(lock=int(g.GNSS_LockState), sats=int(g.SatsNum),
                        docxo=int(g.DOCXO_LockState), time='')
        except Exception:
            return {}

    # ---------------- 会话宿主 ----------------
    def set_session(self, session) -> None:
        self.session = session
        self.state.mode = session.name if session else 'std'

    def step(self):
        """publisher 单步: 转发给当前会话。"""
        return self.session.step() if self.session else None
