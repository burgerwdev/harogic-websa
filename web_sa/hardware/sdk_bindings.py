"""
hardware/sdk_bindings.py

ctypes 绑定层 —— 整个项目中唯一直接触碰 libhtraapi (dll) 的模块。
业务层只通过本模块暴露的 API/结构体工作；mock 或硬件替换只需重写本模块。

来源: web_sa/server.py (v0.11.1) 迁移, 含手动声明的 PNM 结构(官方 Python 包装未导出)。
"""
from __future__ import annotations

import os
import sys as _sys
from ctypes import (
    POINTER,
    Structure,
    byref,
    c_double,
    c_float,
    c_int,
    c_int16,
    c_uint8,
    c_uint16,
    c_uint32,
    c_uint64,
    c_void_p,
    create_string_buffer,
    pointer,
)

_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))))  # ../.. = Python_Examples (htra_api.py)
import htra_api  # 官方 Python 包装 (../htra_api.py)

dll = htra_api.dll

# ---------------------------------------------------------------------------
# PNM 相噪结构 (htra_api.py 未导出, 手动声明 —— 与 /opt/htraapi/inc/htra_api_pnm.h 对齐)
# 踩坑记录: 字段遗漏会导致 DLL 越界写 → 内存损坏/GC 崩溃
# ---------------------------------------------------------------------------
class PNM_Profile_TypeDef(Structure):
    _fields_ = [
        ('CenterFreq', c_double), ('Threshold', c_float), ('RBWRatio', c_double),
        ('StartOffsetFreq', c_double), ('StopOffsetFreq', c_double), ('TraceAverage', c_uint32),
    ]


class PNM_MeasInfo_TypeDef(Structure):
    _fields_ = [
        ('Segments', c_uint32), ('TracePoints', c_uint32), ('PartialUpdateCounts', c_uint32),
        ('FramesInSegment', c_uint32 * 7), ('FrameDetRatioOfSegment', c_uint32 * 7),
        ('StartFreqOfSegment', c_double * 7), ('StopFreqOfSegment', c_double * 7),
        ('RBWOfSegment', c_double * 7),
    ]


class PNM_AuxInfo_TypeDef(Structure):
    """MeasAuxInfo 的 PNM 用副本 —— 含 RFState(uint16), 不可省略。"""
    _fields_ = [
        ('MaxIndex', c_uint32), ('MaxPower_dBm', c_float), ('Temperature', c_int16),
        ('RFState', c_uint16),
        ('SysTimeStamp', c_double), ('AbsoluteTimeStamp', c_double),
        ('Latitude', c_float), ('Longitude', c_float), ('Altitude', c_float),
        ('SATHealth', c_float), ('IFAGCGain', c_double), ('RefClkFreqOffset', c_double),
        ('nsSinceEpoch', c_uint64),
    ]


class Full_MeasAuxInfo(Structure):
    """完整 MeasAuxInfo: 前 11 字段与 htra_api.py 官方包装一致(DLL 兼容),
    追加 .h 尾部字段(IFAGCGain/RefClkFreqOffset/nsSinceEpoch), ppm 由此读取。"""
    _fields_ = [
        ('MaxIndex', c_uint32), ('MaxPower_dBm', c_float), ('Temperature', c_int16),
        ('RFState', c_uint16), ('BBState', c_uint16), ('GainPattern', c_uint16),
        ('ConvertPattern', c_uint32),
        ('SysTimeStamp', c_double), ('AbsoluteTimeStamp', c_double),
        ('Latitude', c_float), ('Longitude', c_float),
        ('IFAGCGain', c_double), ('RefClkFreqOffset', c_double), ('nsSinceEpoch', c_uint64),
    ]


def _bind_pnm() -> bool:
    """绑定 PNM 函数; 库不支持时返回 False。"""
    try:
        dll.PNM_ProfileDeInit.argtypes = [POINTER(c_void_p), POINTER(PNM_Profile_TypeDef)]
        dll.PNM_ProfileDeInit.restype = c_int
        dll.PNM_Configuration.argtypes = [POINTER(c_void_p), POINTER(PNM_Profile_TypeDef),
                                          POINTER(PNM_Profile_TypeDef), POINTER(PNM_MeasInfo_TypeDef)]
        dll.PNM_Configuration.restype = c_int
        dll.PNM_StartMeasure.argtypes = [POINTER(c_void_p)]
        dll.PNM_StartMeasure.restype = c_int
        dll.PNM_StopMeasure.argtypes = [POINTER(c_void_p)]
        dll.PNM_StopMeasure.restype = c_int
        dll.PNM_GetPartialUpdatedFullTrace.argtypes = [
            POINTER(c_void_p), POINTER(c_double), POINTER(c_float), POINTER(c_double),
            POINTER(c_float), POINTER(c_uint32), POINTER(PNM_AuxInfo_TypeDef), POINTER(c_float)]
        dll.PNM_GetPartialUpdatedFullTrace.restype = c_int
        return True
    except Exception:
        return False


# 关键: DSP_InterceptSpectrum 官方包装缺少 argtypes, 必须手动补
dll.DSP_InterceptSpectrum.argtypes = [
    c_double, c_double, POINTER(c_double), POINTER(c_float), c_uint32,
    POINTER(c_double), POINTER(c_float), POINTER(c_uint32)]
dll.DSP_InterceptSpectrum.restype = None

# SWP_GetFullSweep 的 MeasAuxInfo 用完整结构(覆盖 htra_api 的 argtypes)
dll.SWP_GetFullSweep.argtypes = [POINTER(c_void_p), POINTER(c_double),
                                 POINTER(c_float), POINTER(Full_MeasAuxInfo)]

# 参考时钟校准(htra_api.py 未导出; 注意 argtypes 必须完整, 否则指针截断崩溃)
dll.Device_CalibrateRefClock.argtypes = [
    POINTER(c_void_p), c_int, c_double, c_uint64, c_uint8, POINTER(c_double)]
dll.Device_CalibrateRefClock.restype = c_int

PNM_SUPPORTED = _bind_pnm()

# 便捷别名 (业务层使用)
SWP_Profile_TypeDef = htra_api.SWP_Profile_TypeDef
SWP_TraceInfo_TypeDef = htra_api.SWP_TraceInfo_TypeDef
SWP_FreqAssignment_TypeDef = htra_api.SWP_FreqAssignment_TypeDef
SweepTimeMode_TypeDef = htra_api.SweepTimeMode_TypeDef
TracePointsStrategy_TypeDef = htra_api.TracePointsStrategy_TypeDef
TraceAlign_TypeDef = htra_api.TraceAlign_TypeDef
SpurRejection_TypeDef = htra_api.SpurRejection_TypeDef
RBWMode_TypeDef = htra_api.RBWMode_TypeDef
VBWMode_TypeDef = htra_api.VBWMode_TypeDef
Window_TypeDef = htra_api.Window_TypeDef
PreamplifierState_TypeDef = htra_api.PreamplifierState_TypeDef
GainStrategy_TypeDef = htra_api.GainStrategy_TypeDef
ReferenceClockSource_TypeDef = htra_api.ReferenceClockSource_TypeDef
GNSSInfo_TypeDef = htra_api.GNSSInfo_TypeDef
MeasAuxInfo_TypeDef = htra_api.MeasAuxInfo_TypeDef
DeviceInfo_TypeDef = htra_api.DeviceInfo_TypeDef
BootProfile_TypeDef = htra_api.BootProfile_TypeDef
BootInfo_TypeDef = htra_api.BootInfo_TypeDef

__all__ = [
    'PNM_SUPPORTED',
    'BootInfo_TypeDef',
    'BootProfile_TypeDef',
    'DeviceInfo_TypeDef',
    'GNSSInfo_TypeDef',
    'GainStrategy_TypeDef',
    'MeasAuxInfo_TypeDef',
    'PNM_AuxInfo_TypeDef',
    'PNM_MeasInfo_TypeDef',
    'PNM_Profile_TypeDef',
    'PreamplifierState_TypeDef',
    'RBWMode_TypeDef',
    'ReferenceClockSource_TypeDef',
    'SWP_FreqAssignment_TypeDef',
    'SWP_Profile_TypeDef',
    'SWP_TraceInfo_TypeDef',
    'SpurRejection_TypeDef',
    'SweepTimeMode_TypeDef',
    'TraceAlign_TypeDef',
    'TracePointsStrategy_TypeDef',
    'VBWMode_TypeDef',
    'Window_TypeDef',
    'byref',
    'create_string_buffer',
    'dll',
    'pointer',
]
