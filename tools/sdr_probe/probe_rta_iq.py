#!/usr/bin/env python3
"""Probe RTA_GetIQStream (RTA mode IQ look-back) on the SAN-90.

Result on this unit (MFW/FFW 14184, API 0.55.89): the exported functions exist and
RTA_SetDataFormat / RTA_SetLookBackCmd / RTA_Configuration / RTA_TriggerStart all return 0,
and the full 50.781 MHz RTA frame is delivered (PacketCount spectrum packets), but
RTA_GetIQStream always returns -10 (BusTimeOut) - even with LookBack_On and after draining
the whole frame per the API guide. So RTA IQ look-back is NOT usable here; use RTA for the
wide realtime spectrum and IQS for the demod.

Run with WebSA stopped.
"""
import sys
import time

sys.path.insert(0,'/home/hui/git/harogic-websa')
import ctypes as C
from ctypes import byref, c_float, c_int, c_void_p

import htra_api as T
from web_sa.hardware.sdk_bindings import Full_MeasAuxInfo

T.dll.RTA_SetDataFormat.argtypes=[T.POINTER(c_void_p),T.POINTER(c_int)]
T.dll.RTA_SetLookBackCmd.argtypes=[T.POINTER(c_void_p),T.POINTER(c_int)]
T.dll.RTA_GetIQStream.argtypes=[T.POINTER(c_void_p),c_void_p,T.POINTER(c_float),T.POINTER(T.TriggerInfo_TypeDef),T.POINTER(Full_MeasAuxInfo)]
T.dll.RTA_GetRealTimeSpectrum.argtypes=[T.POINTER(c_void_p),T.POINTER(T.c_uint8),T.POINTER(T.c_uint16),T.POINTER(T.RTA_PlotInfo_TypeDef),T.POINTER(T.TriggerInfo_TypeDef),T.POINTER(Full_MeasAuxInfo)]
T.dll.RTA_TriggerStart.argtypes=[T.POINTER(c_void_p)]
dev=c_void_p(); bp=T.BootProfile_TypeDef(); bi=T.BootInfo_TypeDef()
bp.DevicePowerSupply=T.DevicePowerSupply_TypeDef.USBPortAndPowerPort; bp.PhysicalInterface=T.PhysicalInterface_TypeDef.USB
print('open', T.dll.Device_Open(byref(dev),c_int(0),byref(bp),byref(bi)))
def try_cfg(dec, fmt):
    f=c_int(fmt); T.dll.RTA_SetDataFormat(byref(dev),byref(f))
    lk=c_int(1); T.dll.RTA_SetLookBackCmd(byref(dev),byref(lk))
    p=T.RTA_Profile_TypeDef(); T.dll.RTA_ProfileDeInit(byref(dev),byref(p))
    p.CenterFreq_Hz=101.7e6; p.DecimateFactor=dec; p.RefLevel_dBm=0.0
    p.TriggerSource=T.RTA_TriggerSource_TypeDef.Bus; p.TriggerMode=T.TriggerMode_TypeDef.FixedPoints
    p.SweepTimeMode=T.SweepTimeMode_TypeDef.SWTMode_minSWT
    o=T.RTA_Profile_TypeDef(); info=T.RTA_FrameInfo_TypeDef()
    st=T.dll.RTA_Configuration(byref(dev),byref(p),byref(o),byref(info))
    return st,o,info
for dec,fmt in ((256,0),(256,3),(16,0),(16,3)):
    st,o,info=try_cfg(dec,fmt)
    tr=(T.c_uint8*(int(info.PacketValidPoints)+4096))(); bm=(T.c_uint16*(int(info.FrameHeight)*int(info.FrameWidth)+65536))()
    plot=T.RTA_PlotInfo_TypeDef(); aux=Full_MeasAuxInfo(); iq=(T.c_uint8*262144)(); scale=c_float(0.0)
    t0=time.time(); r1=T.dll.RTA_TriggerStart(byref(dev))
    sst=[]
    for _ in range(int(info.PacketCount)):
        sst.append(T.dll.RTA_GetRealTimeSpectrum(byref(dev),tr,bm,byref(plot),byref(T.RTA_TriggerInfo_TypeDef()),byref(aux)))
    iqst=[]; tot=0; tg=T.TriggerInfo_TypeDef()
    for _ in range(int(info.PacketCount) + 1):
        r = T.dll.RTA_GetIQStream(byref(dev), C.cast(iq, c_void_p), byref(scale), byref(tg), byref(aux))
        iqst.append(r)
        if r == 0:
            tot += int(tg.InPacketTriggeredDataSize)
        if r == -301:
            break
    print(f'dec={dec} fmt={fmt}: cfg={st} trig={r1} specSt={sst[:3]}.. iqSt={iqst} iqBytes={tot} scale={scale.value:.2e} t={time.time()-t0:.2f}s')
T.dll.Device_Close(byref(dev))
print('DONE')
