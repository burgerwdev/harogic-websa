"""
hardware/sdk_bindings.py

ctypes binding layer -- the only module in the whole project that directly touches
libhtraapi (dll). The business layer works only through the APIs/structs exposed by
this module; mocking or replacing the hardware only requires rewriting this module.

Source: migrated from web_sa/server.py (v0.11.1), incl. manually declared PNM structs
(not exported by the official Python wrapper).
"""
from __future__ import annotations

import os
import sys as _sys
from ctypes import (
    POINTER,
    Structure,
    byref,
    c_bool,
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
    sizeof,
)

_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))))  # ../.. = Python_Examples (htra_api.py)

# The vendor library runs its FFT/DSP through an OpenMP thread pool. With the default
# "active" wait policy those worker threads busy-spin between calls, so once the SDR
# panadapter calls the vendor FFT continuously they burned ~3 extra cores (measured
# 330%% CPU vs 25%% with a passive policy, same 120 steps/s). These variables are read by
# the OpenMP runtime when the shared library is loaded, so set them before importing it.
os.environ.setdefault('OMP_WAIT_POLICY', 'PASSIVE')
os.environ.setdefault('KMP_BLOCKTIME', '0')

import htra_api  # official Python wrapper (../htra_api.py)

dll = htra_api.dll

# ---------------------------------------------------------------------------
# PNM phase-noise structs (not exported by htra_api.py; manually declared -- aligned
# with /opt/htraapi/inc/htra_api_pnm.h)
# Pitfall notes: a missing field would cause the DLL to write out of bounds -> memory
# corruption / GC crashes
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
    """PNM-specific copy of MeasAuxInfo - includes RFState (uint16), must not be omitted."""
    _fields_ = [
        ('MaxIndex', c_uint32), ('MaxPower_dBm', c_float), ('Temperature', c_int16),
        ('RFState', c_uint16),
        ('SysTimeStamp', c_double), ('AbsoluteTimeStamp', c_double),
        ('Latitude', c_float), ('Longitude', c_float), ('Altitude', c_float),
        ('SATHealth', c_float), ('IFAGCGain', c_double), ('RefClkFreqOffset', c_double),
        ('nsSinceEpoch', c_uint64),
    ]


class Full_MeasAuxInfo(Structure):
    """Full MeasAuxInfo.

    The official wrapper in htra_api.py stops after Longitude (48 bytes) and the previous
    hand-written version stopped after Longitude too (72 bytes), but the header also has
    Altitude + SATHealth before IFAGCGain - so the struct was 72 instead of 80 and
    SWP_GetFullSweep / RTA_GetRealTimeSpectrum wrote 8 bytes past it on EVERY call
    (native heap corruption -> "corrupted size vs. prev_size"). Verified against
    sizeof(MeasAuxInfo_TypeDef) compiled from /opt/htraapi/inc/htra_api.h.
    """
    _fields_ = [
        ('MaxIndex', c_uint32), ('MaxPower_dBm', c_float), ('Temperature', c_int16),
        ('RFState', c_uint16), ('BBState', c_uint16), ('GainPattern', c_uint16),
        ('ConvertPattern', c_uint32),
        ('SysTimeStamp', c_double), ('AbsoluteTimeStamp', c_double),
        ('Latitude', c_float), ('Longitude', c_float),
        ('Altitude', c_float), ('SATHealth', c_float),
        ('IFAGCGain', c_double), ('RefClkFreqOffset', c_double), ('nsSinceEpoch', c_uint64),
    ]


assert sizeof(Full_MeasAuxInfo) == 80, (
    'Full_MeasAuxInfo size %d != 80: a short struct makes the DLL write past it.'
    % sizeof(Full_MeasAuxInfo))


# ---------------------------------------------------------------------------
# SDR support: IQS streaming (IQS_*), digital down-conversion (DSP_DDC_*),
# analog demod (ADM_*), and the hardware capability word. Structs marked
# "declared here" are absent from the official htra_api.py wrapper.
# ---------------------------------------------------------------------------
class HardWareState_TypeDef(Structure):
    """Device_GetHardwareState (not exported by htra_api.py). Enums are c_int."""
    _fields_ = [
        ('GNSSPeriphType', c_int), ('GNSSType', c_int), ('OCXOType', c_int),
        ('InternalOCXO', c_uint8), ('SignalSourceEn', c_uint8),
        ('ADC_VariableRateEn', c_uint8), ('IM3_filter', c_uint8),
    ]


class AMDemodParam_TypeDef(Structure):
    """ADM_AMDemod_PM1 output (aligned with htra_api.h)."""
    _fields_ = [
        ('DemodWaveform', POINTER(c_float)), ('AFSpectrum_ModDepth', POINTER(c_float)),
        ('AFSpectrum_Freq', POINTER(c_double)), ('DemodWaveformSize', c_uint32),
        ('ModDepth', c_float), ('ModDepthPeakPos', c_float), ('ModDepthPeakNeg', c_float),
        ('ModDepthHalfPeak', c_float), ('ModDepthRMS', c_float), ('CarrierPower', c_float),
        ('ModRate', c_double), ('SINAD', c_float), ('RMSPower', c_float),
        ('FreqError', c_double), ('SNR', c_float), ('DistTotalVrms', c_float),
        ('THD', c_float), ('PEP', c_float),
    ]


class FMDemodParam_TypeDef(Structure):
    """ADM_FMDemod_PM1 output (aligned with htra_api.h)."""
    _fields_ = [
        ('DemodWaveform', POINTER(c_float)), ('AFSpectrum_Deviation', POINTER(c_float)),
        ('AFSpectrum_Freq', POINTER(c_double)), ('DemodWaveformSize', c_uint32),
        ('Deviation', c_float), ('DeviationPeakPos', c_float), ('DeviationPeakNeg', c_float),
        ('DeviationHalfPeak', c_float), ('DeviationRMS', c_float), ('CarrierPower', c_float),
        ('CarrierFreqErr', c_double), ('ModRate', c_double), ('SINAD', c_float),
        ('SNR', c_float), ('DistTotalVrms', c_float), ('THD', c_float),
    ]


def _bind_sdr() -> dict:
    """Bind IQS/DDC/ADM/hardware-state entry points not covered by htra_api.py."""
    caps = {'adm': False, 'ddc_delay': False, 'hw_state': False}
    try:
        dll.DSP_DDC_GetDelay.argtypes = [POINTER(c_void_p), POINTER(c_uint32)]
        dll.DSP_DDC_GetDelay.restype = None
        caps['ddc_delay'] = True
    except Exception:
        pass
    try:
        dll.ADM_Open.argtypes = [POINTER(c_void_p)]
        dll.ADM_Open.restype = None
        dll.ADM_Close.argtypes = [POINTER(c_void_p)]
        dll.ADM_Close.restype = None
        dll.ADM_AMDemod.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64,
                                    c_double, POINTER(c_float)]
        dll.ADM_AMDemod.restype = c_int
        dll.ADM_AMDemod_PM1.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64,
                                        c_double, c_float, POINTER(AMDemodParam_TypeDef)]
        dll.ADM_AMDemod_PM1.restype = c_int
        dll.ADM_FMDemod.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64,
                                    c_double, c_bool, POINTER(c_float)]
        dll.ADM_FMDemod.restype = c_int
        dll.ADM_FMDemod_PM1.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64,
                                        c_double, c_float, c_bool, POINTER(FMDemodParam_TypeDef)]
        dll.ADM_FMDemod_PM1.restype = c_int
        caps['adm'] = True
    except Exception:
        pass
    try:
        dll.Device_GetHardwareState.argtypes = [POINTER(c_void_p), POINTER(HardWareState_TypeDef)]
        dll.Device_GetHardwareState.restype = c_int
        caps['hw_state'] = True
    except Exception:
        pass
    return caps


SDR_CAPS = _bind_sdr()

# ---------------------------------------------------------------------------
# Vendor wrapper gap (root cause of the native heap corruption): htra_api.py's
# DeviceState_TypeDef still declares the obsolete `LicenseCode` where the current header
# (/opt/htraapi/inc/htra_api.h) has `nsSinceEpoch` (uint64). That makes the struct - and
# the IQStream_TypeDef embedding it - 8 bytes too small (720 vs 728), so
# IQS_GetIQStream_PM1 writes 8 bytes past the end of our buffer on EVERY packet and
# corrupts the heap (observed as "corrupted size vs. prev_size" / "munmap_chunk(): invalid
# pointer" / SIGABRT, i.e. random worker restarts). Re-declare the stream struct with the
# header-correct size and rebind every entry point that takes it.
# ---------------------------------------------------------------------------
class DeviceState_TypeDef(Structure):
    _fields_ = list(htra_api.DeviceState_TypeDef._fields_[:15]) + [('nsSinceEpoch', c_uint64)]


class IQStream_TypeDef(Structure):
    _fields_ = [
        ('AlternIQStream', POINTER(c_void_p)),
        ('IQS_ScaleToV', c_float),
        ('MaxPower_dBm', c_float),
        ('MaxIndex', c_uint32),
        ('IQS_Profile', htra_api.IQS_Profile_TypeDef),
        ('IQS_StreamInfo', htra_api.IQS_StreamInfo_TypeDef),
        ('IQS_TriggerInfo', htra_api.TriggerInfo_TypeDef),
        ('DeviceInfo', htra_api.DeviceInfo_TypeDef),
        ('DeviceState', DeviceState_TypeDef),
    ]


assert sizeof(IQStream_TypeDef) == 728, (
    'IQStream_TypeDef size %d != 728: the vendor header changed; passing a short struct to '
    'IQS_GetIQStream_PM1 corrupts the heap.' % sizeof(IQStream_TypeDef))

for _name, _argtypes in (
        ('IQS_GetIQStream_PM1', [POINTER(c_void_p), POINTER(IQStream_TypeDef)]),
        ('IQS_GetIQStream_PM2', [POINTER(c_void_p), POINTER(IQStream_TypeDef),
                                 POINTER(Full_MeasAuxInfo)]),
        ('DSP_DDC_Execute', [POINTER(c_void_p), POINTER(IQStream_TypeDef),
                             POINTER(IQStream_TypeDef)]),
        ('DSP_FFT_IQSToSpectrum', [POINTER(c_void_p), POINTER(IQStream_TypeDef),
                                   POINTER(c_double), POINTER(c_float)]),
):
    if hasattr(dll, _name):
        getattr(dll, _name).argtypes = _argtypes


def _bind_pnm() -> bool:
    """Bind the PNM functions; return False if the library does not support them."""
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


# Important: the official wrapper for DSP_InterceptSpectrum is missing argtypes, must add them manually
dll.DSP_InterceptSpectrum.argtypes = [
    c_double, c_double, POINTER(c_double), POINTER(c_float), c_uint32,
    POINTER(c_double), POINTER(c_float), POINTER(c_uint32)]
dll.DSP_InterceptSpectrum.restype = None

# SWP_GetFullSweep's MeasAuxInfo uses the full structure (overrides htra_api's argtypes)
dll.SWP_GetFullSweep.argtypes = [POINTER(c_void_p), POINTER(c_double),
                                 POINTER(c_float), POINTER(Full_MeasAuxInfo)]

# Reference clock calibration (not exported by htra_api.py; note that argtypes must be
# complete, otherwise the pointer truncates and crashes)
dll.Device_CalibrateRefClock.argtypes = [
    POINTER(c_void_p), c_int, c_double, c_uint64, c_uint8, POINTER(c_double)]
dll.Device_CalibrateRefClock.restype = c_int

PNM_SUPPORTED = _bind_pnm()


def _insert_ifagc(base, name):
    """Rebuild a profile struct with the ``EnableIFAGC`` byte the vendor wrapper omits.

    htra_api.py is stale for the SWP/RTA/DET profiles: the C header has

        int8_t  Atten;
        uint8_t EnableIFAGC;   <-- missing from the wrapper
        <enum>  <next>;       <-- 4-byte aligned, so the byte lands in padding

    The field list is rebuilt from the wrapper's own field types with the byte restored.
    Every field offset is asserted to be unchanged: if the byte had NOT been absorbed by
    alignment padding, some offset (or the size) would move and the assertion would fire.
    That is why the omission never corrupted memory - it only made IF AGC unsettable.
    """
    fields = []
    inserted = False
    for fname, ftype in base._fields_:
        fields.append((fname, ftype))
        if fname == 'Atten':
            fields.append(('EnableIFAGC', c_uint8))
            inserted = True
    if not inserted:
        raise AssertionError(f'{name}: no Atten field to anchor EnableIFAGC to')
    cls = type(name, (Structure,), {'_fields_': fields})
    if sizeof(cls) != sizeof(base):
        raise AssertionError(
            f'{name}: size changed {sizeof(cls)} != {sizeof(base)}; EnableIFAGC is not in '
            'padding - the struct must be re-declared field by field instead')
    for fname, _ in base._fields_:
        if getattr(cls, fname).offset != getattr(base, fname).offset:
            raise AssertionError(f'{name}.{fname} moved off its C offset')
    return cls


SWP_Profile_TypeDef = _insert_ifagc(htra_api.SWP_Profile_TypeDef, 'SWP_Profile_TypeDef')
RTA_Profile_TypeDef = _insert_ifagc(htra_api.RTA_Profile_TypeDef, 'RTA_Profile_TypeDef')
DET_Profile_TypeDef = _insert_ifagc(htra_api.DET_Profile_TypeDef, 'DET_Profile_TypeDef')
dll.SWP_ProfileDeInit.argtypes = [POINTER(c_void_p), POINTER(SWP_Profile_TypeDef)]
dll.SWP_Configuration.argtypes = [POINTER(c_void_p), POINTER(SWP_Profile_TypeDef),
                                  POINTER(SWP_Profile_TypeDef),
                                  POINTER(htra_api.SWP_TraceInfo_TypeDef)]
dll.RTA_ProfileDeInit.argtypes = [POINTER(c_void_p), POINTER(RTA_Profile_TypeDef)]
dll.RTA_Configuration.argtypes = [POINTER(c_void_p), POINTER(RTA_Profile_TypeDef),
                                 POINTER(RTA_Profile_TypeDef),
                                 POINTER(htra_api.RTA_FrameInfo_TypeDef)]
dll.DET_ProfileDeInit.argtypes = [POINTER(c_void_p), POINTER(DET_Profile_TypeDef)]
dll.DET_Configuration.argtypes = [POINTER(c_void_p), POINTER(DET_Profile_TypeDef),
                                 POINTER(DET_Profile_TypeDef),
                                 POINTER(htra_api.DET_StreamInfo_TypeDef)]

# Convenient aliases (used by the business layer)
SWP_TraceInfo_TypeDef = htra_api.SWP_TraceInfo_TypeDef
SWP_FreqAssignment_TypeDef = htra_api.SWP_FreqAssignment_TypeDef
SweepTimeMode_TypeDef = htra_api.SweepTimeMode_TypeDef
TracePointsStrategy_TypeDef = htra_api.TracePointsStrategy_TypeDef
TraceAlign_TypeDef = htra_api.TraceAlign_TypeDef
TraceDetector_TypeDef = htra_api.TraceDetector_TypeDef
TraceDetectMode_TypeDef = htra_api.TraceDetectMode_TypeDef
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

# SDR aliases (IQS / DDC / FFT / demod)
IQS_Profile_TypeDef = htra_api.IQS_Profile_TypeDef
IQS_StreamInfo_TypeDef = htra_api.IQS_StreamInfo_TypeDef
IQStream_TypeDef = IQStream_TypeDef   # header-corrected above (see the note there)
TriggerInfo_TypeDef = htra_api.TriggerInfo_TypeDef
DataFormat_TypeDef = htra_api.DataFormat_TypeDef
TriggerMode_TypeDef = htra_api.TriggerMode_TypeDef
IQS_TriggerSource_TypeDef = htra_api.IQS_TriggerSource_TypeDef
DCCancelerMode_TypeDef = htra_api.DCCancelerMode_TypeDef
QDCMode_TypeDef = htra_api.QDCMode_TypeDef
DSP_DDC_TypeDef = htra_api.DSP_DDC_TypeDef
DSP_FFT_TypeDef = htra_api.DSP_FFT_TypeDef

__all__ = [
    'PNM_SUPPORTED',
    'SDR_CAPS',
    'AMDemodParam_TypeDef',
    'FMDemodParam_TypeDef',
    'HardWareState_TypeDef',
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
    'TraceDetector_TypeDef',
    'TraceDetectMode_TypeDef',
    'TracePointsStrategy_TypeDef',
    'VBWMode_TypeDef',
    'Window_TypeDef',
    'byref',
    'create_string_buffer',
    'dll',
    'pointer',
]
