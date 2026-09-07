#调用必要的Python库
import ctypes
from ctypes import *
import os
import platform
import sys

_arch = {
    'x86_64': 'x86_64',
    'amd64': 'x86_64',
    'aarch64': 'aarch64',
    'arm64': 'aarch64',
    'armv7l': 'armv7',
}.get(platform.machine().lower(), platform.machine().lower())
_default_lib = os.path.join('/opt/htraapi/lib', _arch, 'libhtraapi.so')
dll = ctypes.CDLL(os.getenv('HTRA_API_LIB', _default_lib))

#类型申明
c_int16_p = POINTER(c_int16)

#物理总线类型 BootProfile.PhysicalInterface
class PhysicalInterface_TypeDef(c_int):
    USB = 0x00                      #使用USB作为物理接口，适用于SAE、SAM、SAN等USB接口产品
    QSFP = 0x02                     #使用40Gbps QSFP+ 作为物理接口
    ETH = 0x03                      #使用100M/1000M 以太网 作为物理接口，适用于NXE、NXM、NXN等以太网接口产品
    HLVDS = 0x04                    #调试专用，请勿使用
    VIRTUAL = 0x07                  #使用虚拟总线，即无物理总线，用于仿真与调试

#设备供电方式 BootProfile.DevicePowerSupply
class DevicePowerSupply_TypeDef(c_int):
    USBPortAndPowerPort = 0x00      #使用USB数据端口及电源端口供电
    USBPortOnly = 0x01              #仅使用USB数据端口
    Others = 0x02                   #其他方式, 当使用ETH总线时，使用此选项

#IP地址版本 BootProfile.IPVersion
class IPVersion_TypeDef(c_int):
    IPv4 = 0x00                     #使用IPv4地址
    IPv6 = 0x01                     #使用IPv6地址

#频率分配方式(SWP)
class SWP_FreqAssignment_TypeDef(c_int):
    StartStop = 0x00                #以起始频率和终止频率来设置频率范围
    CenterSpan = 0x01               #以中心频率和频率扫宽来设置频率范围

#窗类型(SWP\RTA\DSP) —— 与官方 API 编程指南 V0.55.89 (27.15 Window_TypeDef) 一致
class Window_TypeDef(c_int):
    FlatTop = 0x00                  #平顶窗: 良好幅度准确度
    Blackman_Nuttall = 0x01         #Nuttall窗: 良好频率分辨力
    LowSideLobe = 0x02              #低旁瓣窗: 良好抑制频谱泄漏能力(旧头文件名 Blackman)
    Rect = 0x03                     #矩形窗: 等效不加窗(旧头文件名 Hamming)
    Kaiser = 0x04                   #凯泽窗: 主瓣宽度与旁瓣抑制可配置(旧头文件名 Hanning)
    Gaussian_CISPR = 0x05           #CISPR 标准高斯窗, 仅 EMC 模式

#RBW更新方式(SWP)
class RBWMode_TypeDef(c_int):
    RBW_Manual = 0x00               #手动输入RBW
    RBW_Auto = 0x01                 #自动随SPAN更新RBW，RBW = SPAN / 2000
    RBW_OneThousandthSpan = 0x02    #强制 RBW = 0.001 * SPAN
    RBW_OnePercentSpan = 0x03       #强制 RBW = 0.01 * SPAN

#VBW更新方式(SWP)
class VBWMode_TypeDef(c_int):
    VBW_Manual = 0x00               #手动输入VBW
    VBW_EqualToRBW = 0x01           #强制 VBW = RBW
    VBW_TenPercentRBW = 0x02        #强制 VBW = 0.1 * RBW
    VBW_OnePercentRBW = 0x03        #强制 VBW = 0.01 * RBW
    VBW_TenTimesRBW = 0x04          #强制 VBW = 10 * RBW，完全旁路VBW滤波器

#扫描时间配置模式(SWP)
class SweepTimeMode_TypeDef(c_int):
    SWTMode_minSWT = 0x00           #以最短扫描时间进行扫描
    SWTMode_minSWTx2 = 0x01         #以近似2倍最短扫描时间进行扫描
    SWTMode_minSWTx4 = 0x02         #以近似4倍最短扫描时间进行扫描
    SWTMode_minSWTx10 = 0x03        #以近似10倍最短扫描时间进行扫描
    SWTMode_minSWTx20 = 0x04        #以近似20倍最短扫描时间进行扫描
    SWTMode_minSWTx50 = 0x05        #以近似50倍最短扫描时间进行扫描
    SWTMode_minSWTxN = 0x06         #以近似N倍最短扫描时间进行扫描，N等于SweepTimeMultiple
    SWTMode_Manual = 0x07           #以近似指定的扫描时间进行扫描，扫描时间等于SweepTime
    SWTMode_minSMPxN = 0x08         #以近似N倍最短采样时间进行单个频点的采样，N等于SampleTimeMultiple

#频点内多帧检波方式(SWP\RTA)
class Detector_TypeDef(c_int):
    Detector_Sample  = 0x00         #每个频点的功率谱间不进行帧间检波
    Detector_PosPeak = 0x01         #每个频点的功率谱间进行帧检波，最终输出一帧；帧与帧取MaxHold
    Detector_Average = 0x02         #每个频点的功率谱间进行帧检波，最终输出一帧；帧与帧取平均
    Detector_NegPeak = 0x03         #每个频点的功率谱间进行帧检波，最终输出一帧，帧与帧取MinHold
    Detector_MaxPower = 0x04        #在FFT前，每个频点都进行长时间的采样，从中选取功率最大的帧数据进行FFT，用于捕获脉冲等瞬时信号（仅SWP模式可用）
    Detector_RawFrames = 0x05       #每个频点均进行多次采样，多次FFT分析，并逐帧输出功率谱（仅SWP模式可用）
    Detector_RMS = 0x06             #每个频点的功率谱间进行帧检波，最终输出一帧；帧与帧取RMS
    Detector_AutoPeak = 0x07        #每个频点的功率谱间进行帧检波，最终输出一帧；帧与帧取AutoPeak

#频率检波方式(DSP)
class TraceFormat_TypeDef(c_int):
    TraceFormat_Standard = 0x00     #频率等间隔
    TraceFormat_PrecisFrq = 0x01    #频率准确

#迹线检波方式(SWP\RTA\DSP)
class TraceDetectMode_TypeDef(c_int):   
    TraceDetectMode_Auto = 0x00     #自动选择迹线检波模式
    TraceDetectMode_Manual = 0x01   #指定迹线检波模式

#迹线检波方式(SWP\RTA\DSP)
class TraceDetector_TypeDef(c_int):
    TraceDetector_AutoSample = 0x00 #自动取样检波
    TraceDetector_Sample = 0x01     #随机检波
    TraceDetector_PosPeak = 0x02    #正峰值检波
    TraceDetector_NegPeak = 0x03    #负峰值检波
    TraceDetector_RMS = 0x04        #均方根检波
    TraceDetector_Bypass = 0x05     #不执行检波
    TraceDetector_AutoPeak = 0x06   #自动峰值检波

#迹线点数逼近方式(SWP)
class TracePointsStrategy_TypeDef(c_int):
    SweepSpeedPreferred = 0x00      #优先保证扫描速度最快，然后尽量靠近设置的目标迹线点数
    PointsAccuracyPreferred = 0x01  #优先保证实际迹线点数接近设置的目标迹线点数
    BinSizeAssined = 0x02           #优先保证按照设定的频率间隔来生成迹线

#迹线对齐方式(SWP)
class TraceAlign_TypeDef(c_int):
    NativeAlign = 0x00              #自然对齐
    AlignToStart = 0x01             #对齐至起始频率
    AlignToCenter = 0x02            #对齐至中心频率

#FFT平台(SWP)
class FFTExecutionStrategy_TypeDef(c_int):
    Auto = 0x00                     #根据设置自动选择使用CPU还是FPGA进行FFT计算
    Auto_CPUPreferred = 0x01        #根据设置自动选择使用CPU还是FPGA进行FFT计算，CPU优先
    Auto_FPGAPreferred = 0x02       #根据设置自动选择使用CPU还是FPGA进行FFT计算，FPGA优先
    CPUOnly_LowResOcc = 0x03        #强制使用CPU计算，低资源占用，最大FFT点数256K
    CPUOnly_MediumResOcc = 0x04     #强制使用CPU计算，中资源占用，最大FFT点数1M
    CPUOnly_HighResOcc = 0x05       #强制使用CPU计算，高资源占用，最大FFT点数4M
    FPGAOnly = 0x06                 #强制使用FPGA计算                 

#收发模式下收发端口状态:前者为发，后者为收(仅适用于TRx 系列)(VNA)
class RxPort_TypeDef(c_int):
    ExternalPort = 0x00             #外部端口
    InternalPort = 0x01             #内部端口
    ANT_Port = 0x02                 #only for TRx series
    TR_Port = 0x03                  #only for TRx series
    SWR_Port = 0x04                 #only for TRx series
    INT_Port = 0x05                 #only for TRx series

#杂散抑制类型(SWP)
class SpurRejection_TypeDef(c_int):
    Bypass = 0x00                   #不进行杂散抑制
    Standard = 0x01                 #中级杂散抑制
    Enhanced = 0x02                 #高级杂散抑制

#系统参考时钟(all)
class ReferenceClockSource_TypeDef(c_int):
    ReferenceClockSource_Internal  = 0x00           #内部时钟源(默认10MHz)
    ReferenceClockSource_External = 0x01            #外部时钟源(默认10MHz)，当系统检测到外部时钟无法锁定时，将自动切换至内部参考
    ReferenceClockSource_Internal_Premium = 0x02    #内部时钟源-高品质，选择DOCXO或OCXO
    ReferenceClockSource_External_Forced = 0x03     #外部时钟源，并且无视锁定情况，即使失锁也不会切换至内部参考

#系统时钟(all)
class SystemClockSource_TypeDef(c_int):
    SystemClockSource_Internal = 0x00   #内部系统时钟源
    SystemClockSource_External = 0x01   #外部系统时钟源

#扫描模式触发源与触发模式(SWP)
class SWP_TriggerSource_TypeDef(c_int):
    InternalFreeRun = 0x00              #内部触发自由运行
    ExternalPerHop = 0x01               #外部触发，每一次触发都跳一个频点
    ExternalPerSweep = 0x02             #外部触发，每一次触发都刷新一条迹线
    ExternalPerProfile = 0x03           #外部触发，每一次触发都应用一个新配置

#触发输入边沿(all)
class TriggerEdge_TypeDef(c_int):
    RisingEdge = 0x00                   #上升沿触发
    FallingEdge = 0x01                  #下降沿触发
    DoubleEdge = 0x02                   #双边沿触发

#触发输出类型(all)
class TriggerOutMode_TypeDef(c_int):
    NNone = 0x00                    #无触发输出
    PerHop = 0x01                   #随每次跳频完成时输出
    PerSweep = 0x02                 #随每次扫描完成时输出
    PerProfile = 0x03               #随每次配置切换输出

#触发输出信号极性(all)
class TriggerOutPulsePolarity_TypeDef(c_int):
    Positive = 0x00                 #正极型脉冲
    Negative = 0x01                 #负极型脉冲

#增益方式(all)
class GainStrategy_TypeDef(c_int):
    LowNoisePreferred = 0x00        #侧重低噪声
    HighLinearityPreferred = 0x01   #侧重高线性度

#预选放大器(all)
class PreamplifierState_TypeDef(c_int):
    AutoOn = 0x00                   #自动使能前置放大器
    ForcedOff = 0x01                #强制保持前置放大器关闭

#迹线更新方式(SWP)
class SWP_TraceType_TypeDef(c_int):
    ClearWrite = 0x00               #输出正常迹线
    MaxHold = 0x01                  #输出经过最大值保持的迹线
    MinHold = 0x02                  #输出经过最小值保持的迹线
    ClearWriteWithIQ = 0x03         #同时输出当前频点的时域数据与频域数据

#本振优化(all)
class LOOptimization_TypeDef(c_int):
    LOOpt_Auto = 0x00               #本振优化，自动
    LOOpt_Speed = 0x01              #本振优化，高扫速
    LOOpt_Spur = 0x02               #本振优化，低杂散
    LOOpt_PhaseNoise = 0x03         #本振优化，低相噪

#流模式数据格式类型(IQS/DSP)
class DataFormat_TypeDef(c_int):
    Complex16bit = 0x00             #IQ，单路数据16位
    Complex32bit = 0x01             #IQ，单路数据32位
    Complex8bit = 0x02              #IQ，单路数据8位
    Complexfloat = 0x06             #IQ，单路数据32位浮点（IQS模式不可用，仅由DDC函数输出数据时回写该枚举变量）

#DSP运算平台(SWP)
class DSPPlatform_Typedef(c_int):
    CPU_DSP  = 0x00                 #在CPU进行计算
    FPGA_DSP = 0x01                 #在FPGA进行计算

#定频点模式（IQS\RTA\DET）触发源
class IQS_TriggerSource_TypeDef(c_int):
    FreeRun = 0x00                  #自由运行
    External = 0x01                 #外部触发。由连接至设备外触发输入端口的物理信号进行触发
    Bus = 0x02                      #总线触发。由函数（指令）的方式进行触发
    Level = 0x03                    #电平触发。设备根据设定的电平门限对输入信号进行检测，当输入超过门限值后自动发起触发
    Timer = 0x04                    #定时器触发。使用设备内部定时器以设定的时间周期进行自动触发
    TxSweep = 0x05                  #信号源扫描的输出触发；当选择此触发源时，采集过程将由信号源扫描的输出触发信号进行触发
    MultiDevSyncByExt = 0x06        #在外部触发信号到来时，多机做同步触发行为
    MultiDevSyncByGNSS1PPS = 0x07   #在GNSS-1PPS到来时，多机做同步触发行为
    SpectrumMask = 0x08             #频谱模板触发，仅RTA模式下可用。暂未开放
    GNSS1PPS = 0x09                 #使用系统内GNSS提供的1PPS进行触发

#触发模式(IQS\RTA\DET)
class TriggerMode_TypeDef(c_int):
    FixedPoints = 0x00              #触发后获取定点长度的数据
    Adaptive = 0x01                 #触发后持续获取数据

#定时器触发与外触发边沿同步
class TriggerTimerSync_TypeDef(c_int):
    NoneSync = 0x00                         #定时器触发不与外触发同步
    SyncToExt_RisingEdge = 0x01             #定时器触发与外触发上升沿同步 
    SyncToExt_FallingEdge = 0x02            #定时器触发与外触发下降沿同步
    SyncToExt_SingleRisingEdge = 0x03       #定时器触发与外触发上升沿单次同步（需要调用指令函数，执行单次同步）
    SyncToExt_SingleFallingEdge = 0x04      #定时器触发与外触发上升沿单次同步（需要调用指令函数，执行单次同步）
    SyncToGNSS1PPS_RisingEdge = 0x05        #定时器触发与GNSS-1PPS上升沿同步 
    SyncToGNSS1PPS_FallingEdge = 0x06       #定时器触发与GNSS-1PPS下降沿同步
    SyncToGNSS1PPS_SingleRisingEdge	= 0x07  #定时器触发与GNSS-1PPS上升沿单次同步（需要调用指令函数，执行单次同步）
    SyncToGNSS1PPS_SingleFallingEdge = 0x08 #定时器触发与GNSS-1PPS上升沿单次同步（需要调用指令函数，执行单次同步）

#直流抑制方式(IQS\DET\RTA)
class DCCancelerMode_TypeDef(c_int):
    DCCOff = 0x00                           #关闭直流抑制功能。
    DCCHighPassFilterMode = 0x01            #开启，高通滤波器方式。该模式下有较好的直流抑制效果，但会损伤直流至低频（约100kHz）的信号分量
    DCCManualOffsetMode = 0x02              #开启，手动偏置方式。该模式下需要手动指定偏置值，且抑制效果弱于高通滤波器模式，但不会损伤直流上的信号分量
    DCCAutoOffsetMode = 0x03                #开启，自动偏置方式。

#正交解调校正(IQS\DET\RTA)
class QDCMode_TypeDef(c_int):
    QDCOff = 0x00                           #关闭QDC
    QDCManualMode = 0x01                    #手动QDC，根据给定参数执行QDC
    QDCAutoMode = 0x02                      #自动QDC，系统在每次IQS_Configuration调用时自动执行校准并使用校准所获得的参数

class ASG_Port_TypeDef(c_int):
    ASG_Port_External = 0x00                #外部端口
    ASG_Port_Internal = 0x01                #内部端口
    ASG_Port_ANT = 0x02                     #ANT端口（仅适用于TRx 系列）
    ASG_Port_TR = 0x03                      #TR端口（仅适用于TRx系列）
    ASG_Port_SWR = 0x04                     #SWR端口（仅适用于TRx系列）
    ASG_Port_INT = 0x05                     #INT端口（仅适用于TRx系列）

class ASG_Mode_TypeDef(c_int):
    ASG_Mute = 0x00                         #静音
    ASG_FixedPoint = 0x01                   #定频定功率
    ASG_FrequencySweep = 0x02               #频率扫描
    ASG_PowerSweep = 0x03                   #功率扫描

class ASG_TriggerSource_TypeDef(c_int):
    ASG_TriggerIn_FreeRun = 0x00            #自由运行
    ASG_TriggerIn_External = 0x01           #外触发
    ASG_TriggerIn_Bus = 0x02                #定时器触发

class ASG_TriggerInMode_TypeDef(c_int):
    ASG_TriggerInMode_Null = 0x00           #自由运行
    ASG_TriggerInMode_SinglePoint = 0x01    #单点触发（触发一次进行单次的频率或功率的配置）
    ASG_TriggerInMode_SingleSweep = 0x02    #单次扫描触发（触发一次进行一个周期的扫描）
    ASG_TriggerInMode_Continous = 0x03      #连续扫描触发（触发一次连续工作）

class ASG_TriggerOutMode_TypeDef(c_int):
    ASG_TriggerOutMode_Null = 0x00          #自由运行
    ASG_TriggerOutMode_SinglePoint = 0x01   #单点触发（一次跳频输出一个脉冲）
    ASG_TriggerOutMode_SingleSweep = 0x02   #单次扫描触发（一次扫描输出一个脉冲）

# GNSS天线状态
class GNSSAntennaState_TypeDef(c_int):
    GNSS_AntennaExternal = 0x00   # 外部天线
    GNSS_AntennaInternal = 0x01   # 内部天线

# DOCXO状态
class DOCXOWorkMode_TypeDef(c_int):
    DOCXO_LockMode = 0x00         # 驯服模式
    DOCXO_HoldMode = 0x01         # 跟踪模式


RTA_TriggerSource_TypeDef = IQS_TriggerSource_TypeDef
DET_TriggerSource_TypeDef = IQS_TriggerSource_TypeDef

#启动配置(配置)
class BootProfile_TypeDef(Structure):
    _fields_ = [("PhysicalInterface",PhysicalInterface_TypeDef),    #物理接口选择
                ("DevicePowerSupply", DevicePowerSupply_TypeDef),   #供电方式选择
                ("ETH_IPVersion", IPVersion_TypeDef),               #ETH网际协议版本
                ("ETH_IPAddress", c_uint8 * 16),                    #ETH接口的IP地址
                ("ETH_RemotePort", c_uint16),                       #ETH接口的侦听端口
                ("ETH_ErrorCode", c_int32),                         #ETH接口的返回代码
                ("ETH_ReadTimeOut", c_int32)]                       #ETH接口的读取超时(ms)

#设备信息(返回)
class DeviceInfo_TypeDef(Structure):
    _fields_ = [("DeviceUID",c_uint64),         #设备序列号
                ("Model",c_uint16),             #设备类型
                ("HardwareVersion",c_uint16),   #硬件版本
                ("MFWVersion",c_uint32),        #MCU固件版本
                ("FFWVersion",c_uint32),]       #FPGA固件版本

#启动信息(返回)
class BootInfo_TypeDef(Structure):
    _fields_ = [("DeviceInfo",DeviceInfo_TypeDef),  #设备信息
                ("BusSpeed",c_uint32),              #总线速度信息
                ("BusVersion",c_uint32),            #总线固件版本
                ("APIVersion",c_uint32),            #API版本
                ("ErrorCodes",c_int * 7),           #启动过程中的错误代码清单
                ("Errors",c_int),                   #启动过程中的错误总数
                ("WarningCodes",c_int * 7),         #启动过程中的警告代码清单
                ("Warnings",c_int)]                 #启动过程中的警告总数

#设备状态(高级变量，不建议直接使用)(返回)
class DeviceState_TypeDef(Structure):
    _fields_ = [("Temperature",c_int16),            #设备温度，摄氏度 = 0.01 * Temperature
                ("RFState",c_uint16),               #射频状态
                ("BBState",c_uint16),               #基带状态
                ("AbsoluteTimeStamp",c_double),     #当前数据包对应的绝对时间戳
                ("Latitude",c_float),               #当前数据包对应的纬度坐标,北纬为正数，南纬为负数，以此区分南北纬
                ("Longitude",c_float),              #当前数据包对应的经度坐标,东经为正数，西经为负数，以此区分东西经
                ("GainPattern",c_uint16),           #当前数据包对应频点所使用的增益控制字
                ("RFCFreq",c_int64),                #当前数据包对应频点所使用的射频中心频率
                ("ConvertPattern",c_uint32),        #当前数据包频点所使用的频率变换方式
                ("NCOFTW",c_uint32),                #当前数据包频点所使用的NCO频率字
                ("SampleRate",c_uint32),            #当前数据包频点所使用的等效采样率，等效采样率 = ADC采样率/抽取倍数
                ("CPU_BCFlag",c_uint16),            #CPU-FFT做拼帧时，所需的BC标志位
                ("IFOverflow",c_uint16),            #设备是否中频过载，考虑并BBState或RFState
                ("DecimateFactor",c_uint16),        #当前数据包频点所使用的抽取倍数
                ("OptionState",c_uint16),           #选件状态
                ("LicenseCode",c_int16)]            #许可证代码

#SWP配置结构（基础配置）
class SWP_Profile_TypeDef(Structure):
    _fields_ = [("StartFreq_Hz",c_double),                                  #起始频率
                ("StopFreq_Hz",c_double),                                   #终止频率
                ("CenterFreq_Hz",c_double),                                 #中心频率
                ("Span_Hz",c_double),                                       #扫频宽度
                ("RefLevel_dBm",c_double),                                  #参考电平
                ("RBW_Hz",c_double),                                        #分辨率带宽
                ("VBW_Hz",c_double),                                        #视频带宽
                ("SweepTime",c_double),                                     #当扫频时间模式为Manual时，该参数表示绝对时间；当设置为N时，表示扫描时间倍数
                ("TraceBinSize_Hz",c_double),                               #迹线相邻频点之间的频率间隔
                ("FreqAssignment",SWP_FreqAssignment_TypeDef),              #选择Start/Stop或Center/Span方式设置频率
                ("Window",Window_TypeDef),                                  #FFT分析所使用的窗函数类型
                ("RBWMode",RBWMode_TypeDef),                                #RBW更新模式：手动输入或根据Span自动设置
                ("VBWMode",VBWMode_TypeDef),                                #VBW更新模式：手动输入，或设置为VBW=RBW、VBW=0.1×RBW、VBW=0.01×RBW
                ("SweepTimeMode",SweepTimeMode_TypeDef),                    #扫描时间模式
                ("Detector",Detector_TypeDef),                              #检测器
                ("TraceFormat",TraceFormat_TypeDef),                        #迹线格式
                ("TraceDetectMode",TraceDetectMode_TypeDef),                #迹线检测模式
                ("TraceDetector",TraceDetector_TypeDef),                    #迹线检测器
                ("TracePoints",c_uint32),                                   #迹线数量
                ("TracePointsStrategy",TracePointsStrategy_TypeDef),        #迹线点映射策略
                ("TraceAlign",TraceAlign_TypeDef),                          #指定迹线对齐方式
                ("FFTExecutionStrategy",FFTExecutionStrategy_TypeDef),      #FFT实现策略
                ("RxPort",RxPort_TypeDef),                                  #射频输入端口
                ("SpurRejection",SpurRejection_TypeDef),                    #杂散抑制
                ("ReferenceClockSource",ReferenceClockSource_TypeDef),      #参考时钟源
                ("ReferenceClockFrequency",c_double),                       #参考时钟频率（Hz）
                ("EnableReferenceClockOut",c_uint8),                        #使能参考时钟输出
                ("SystemClockSource",SystemClockSource_TypeDef),            #默认系统时钟源为内部系统时钟，请在厂商指导下使用
                ("ExternalSystemClockFrequency",c_double),                  #外部系统时钟频率（Hz）
                ("TriggerSource",SWP_TriggerSource_TypeDef),                #输入触发源
                ("TriggerEdge",TriggerEdge_TypeDef),                        #输入触发沿
                ("TriggerOutMode",TriggerOutMode_TypeDef),                  #触发输出模式
                ("TriggerOutPulsePolarity",TriggerOutPulsePolarity_TypeDef),#触发输出脉冲极性
                ("PowerBalance",c_uint32),                                  #功耗与扫描速度的平衡
                ("GainStrategy",GainStrategy_TypeDef),                      #增益策略
                ("Preamplifier",PreamplifierState_TypeDef),                 #前置放大器作用设置
                ("AnalogIFBWGrade",c_uint8),                                #中频带宽范围
                ("IFGainGrade",c_uint8),                                    #中频增益档位
                ("EnableDebugMode",c_uint8),                                #调试模式：不建议用于高级应用，默认值为0。
                ("CalibrationSettings",c_uint8),                            #校准选择：不建议用于高级应用，默认值为0。
                ("Atten",c_int8),                                           #校准选择：不建议用于高级应用，默认值为0。
                ("TraceType",SWP_TraceType_TypeDef),                        #输出迹线类型。
                ("LOOptimization",LOOptimization_TypeDef)]                  #本振优化
    
#SWP模式迹线信息结构（返回）
class SWP_TraceInfo_TypeDef(Structure):
    _fields_ = [("FullsweepTracePoints",c_int),         #完整迹线的点数
                ("PartialsweepTracePoints",c_int),      #每个频点对应的迹线点数，即每次GetPart获取的点数
                ("TotalHops",c_int),                    #完整迹线的频点数量，即完成一条完整迹线所需的GetPart调用次数。
                ("UserStartIndex",c_uint32),            #用户指定StartFreq_Hz在迹线数组中的索引，即当HopIndex=0时，Freq[UserStartIndex]为最接近SWPProfile.StartFreq_Hz的频点。
                ("UserStopIndex",c_uint32),             #用户指定StopFreq_Hz在迹线数组中的索引，即当HopIndex=TotalHops-1时，Freq[UserStopIndex]为最接近SWPProfile.StopFreq_Hz的频点。
                ("TraceBinBW_Hz",c_double),             #迹线相邻两点之间的频率间隔。
                ("StartFreq_Hz",c_double),              #迹线第一个频点的频率。
                ("AnalysisBW_Hz",c_double),             #每个频点对应的分析带宽。
                ("TraceDetectRatio",c_int),             #视频检测的检测比例。
                ("DecimateFactor",c_int),               #时域数据抽取倍数。
                ("FrameTimeMultiple",c_float),          #帧分析时间倍数：设备在单一频点的分析时间= 默认分析时间（系统设置）×帧时间倍数。增大帧时间倍数会增加设备最小扫描时间，但不呈严格线性关系。
                ("FrameTime",c_double),                 #帧扫频时间：用于单帧FFT分析的信号时长（单位：秒）。
                ("EstimateMinSweepTime",c_double),      #当前配置下可设置的最小扫描时间（单位：秒），结果主要受Span、RBW、VBW、帧扫频时间等因素影响
                ("DataFormat",DataFormat_TypeDef),      #时域数据格式
                ("SamplePoints",c_uint64),              #时域数据采样长度
                ("GainParameter",c_uint32),             #与增益相关的参数，包括：Space（31～24位）、前置放大器状态（23～16位）、起始射频频段（15～8位）、终止射频频段（7～0位）。
                ("DSPPlatform",DSPPlatform_Typedef)]    #当前配置下的DSP计算平台。

#测量数据辅助信息（返回）
class MeasAuxInfo_TypeDef(Structure):
    _fields_ = [("MaxIndex",c_uint32),                  #当前数据包中最大功率值的索引位置。
                ("MaxPower_dBm",c_float),               #当前数据包中的最大功率值
                ("Temperature",c_int16),                #设备温度（摄氏度）= 0.01 × temperature（原始值）
                ("RFState",c_uint16),                   #射频状态
                ("BBState",c_uint16),                   #基带状态
                ("GainPattern",c_uint16),               #当前数据包对应频点所使用的增益控制字
                ("ConvertPattern",c_uint32),            #当前数据包频点使用的变频模式
                ("SysTimeStamp",c_double),              #当前数据包的系统时间戳（单位：秒）
                ("AbsoluteTimeStamp",c_double),         #当前数据包的绝对时间戳。
                ("Latitude",c_float),                   #当前数据包对应的纬度坐标，北纬为正，南纬为负，用于区分南北纬。
                ("Longitude",c_float)]                  #当前数据包对应的经度坐标，东经为正，西经为负，用于区分东西经。

#IQS 配置结构（配置）
class IQS_Profile_TypeDef(Structure):
    _fields_ = [("CenterFreq_Hz",c_double),                                 #中心频率
                ("RefLevel_dBm",c_double),                                  #参考电平
                ("DecimateFactor",c_uint32),                                #时域抽取因子
                ("RxPort",RxPort_TypeDef),                                  #射频输入端口
                ("BusTimeout_ms",c_uint32),                                 #传输超时
                ("TriggerSource",IQS_TriggerSource_TypeDef),                #输入触发源
                ("TriggerEdge",TriggerEdge_TypeDef),                        #输入触发边沿
                ("TriggerMode",TriggerMode_TypeDef),                        #输入触发模式
                ("TriggerLength",c_uint64),                                 #输入触发后采样点数。仅在FixedPoints模式下生效
                ("TriggerOutMode",TriggerOutMode_TypeDef),                  #触发输出模式
                ("TriggerOutPulsePolarity",TriggerOutPulsePolarity_TypeDef),#触发输出脉冲极性
                ("TriggerLevel_dBm",c_double),                              #电平触发阈值
                ("TriggerLevel_SafeTime",c_double),                         #电平触发防抖保护时间，单位秒
                ("TriggerDelay",c_double),                                  #触发延迟，单位秒
                ("PreTriggerTime",c_double),                                #预触发时间，单位秒
                ("TriggerTimerSync",TriggerTimerSync_TypeDef),              #定时与外触发边沿同步选项。仅在定时触发模式下生效
                ("TriggerTimer_Period",c_double),                           #定时触发的触发周期，单位秒。仅在定时触发模式下生效		
                ("EnableReTrigger",c_uint8),                                #使能设备捕获触发后多次响应。仅在FixedPoints模式下可用
                ("ReTrigger_Period",c_double),                              #触发设备多次响应的时间间隔。在定时器触发模式下也用作触发周期（单位：秒）
                ("ReTrigger_Count",c_uint16),                               #一次触发后，除触发响应外还需进行若干次响应。
                ("DataFormat",DataFormat_TypeDef),                          #数据格式
                ("GainStrategy",GainStrategy_TypeDef),                      #增益策略
                ("Preamplifier",PreamplifierState_TypeDef),                 #前置放大器动作
                ("AnalogIFBWGrade",c_uint8),                                #中频带宽档位
                ("IFGainGrade",c_uint8),                                    #中频增益档位
                ("EnableDebugMode",c_uint8),                                #调试模式：不建议用于高级应用，默认值为0
                ("ReferenceClockSource",ReferenceClockSource_TypeDef),      #参考时钟源
                ("ReferenceClockFrequency",c_double),                       #参考时钟频率
                ("EnableReferenceClockOut",c_uint8),                        #使能参考时钟输出
                ("SystemClockSource",SystemClockSource_TypeDef),            #系统时钟源
                ("ExternalSystemClockFrequency",c_double),                  #外部系统时钟频率（Hz）
                ("NativeIQSampleRate_SPS",c_double),                        #适用于特定设备：原生IQ采样率。对于可变采样率设备，可通过该参数调整采样率；不可变采样率设备始终使用系统默认固定值配置。
                ("EnableIFAGC",c_uint8),                                    #适用于特定设备：中频AGC控制，0：关闭AGC，使用MGC模式；1：启用AGC。
                ("Atten",c_int8),                                           #衰减
                ("DCCancelerMode",DCCancelerMode_TypeDef),                  #适用于特定设备：直流抑制。 0：关闭DCC；1：开启高通滤波模式（抑制效果更好，但会影响DC至100 kHz范围内的信号）；2：开启手动偏置模式（需手动校准，但不会损伤低频信号）。
                ("QDCMode",QDCMode_TypeDef),                                #适用于特定设备：IQ幅相校正器。 QDCOff：关闭QDC功能；QDCManualMode：启用手动模式；QDCAutoMode：启用自动QDC模式。
                ("QDCIGain",c_float),                                       #适用于特定设备：I路归一化线性增益，1.0表示无增益，设置范围0.8～1.2。
                ("QDCQGain",c_float),                                       #适用于特定设备：Q路归一化线性增益，1.0表示无增益，设置范围0.8～1.2。
                ("QDCPhaseComp",c_float),                                   #适用于特定设备：归一化相位补偿系数，设置范围-0.2～+0.2。
                ("DCCIOffset",c_int8),                                      #适用于特定设备：I通道直流偏置，单位LSB。
                ("DCCQOffset",c_int8),                                      #适用于特定设备：Q通道直流偏置，单位LSB。
                ("LOOptimization",LOOptimization_TypeDef)]                  #本振优化

#IQS 配置后返回的流程信息结构（已返回）
class IQS_StreamInfo_TypeDef(Structure):
    _fields_ = [("Bandwidth",c_double),         #当前配置对应接收机的物理通道或数字信号处理带宽。
                ("IQSampleRate",c_double),      #当前配置对应的IQ单通道采样率（单位：Sample/s，采样点/秒）。
                ("PacketCount",c_uint64),       #当前配置对应的数据包总数，仅在FixedPoints（固定点数）模式下生效。
                ("StreamSamples",c_uint64),     #在FixedPoints模式下表示当前配置对应的总采样点数；在Adaptive（自适应）模式下该值无实际意义，固定为0。
                ("StreamDataSize",c_uint64),    #在FixedPoints模式下表示当前配置对应的采样数据总字节数；在Adaptive模式下该值无实际意义，固定为0。
                ("PacketSamples",c_uint32),     #每次调用IQS_GetIQStream获得的数据包中的采样点数（每个数据包所包含的采样点数）。
                ("PacketDataSize",c_uint32),    #每次调用IQS_GetIQStream可获取的有效数据字节数。
                ("GainParameter",c_uint32)]     #与增益相关的参数，包括：Space（31~24位）、前置放大器状态PreAmplifierState（23~16位）、起始射频频段StartRFBand（15~8位）、终止射频频段StopRFBand（7~0位）。

#IQS 数据包中包含的触发信息结构，DET 与 RTA 的触发信息返回结构相同（返回）
class TriggerInfo_TypeDef(Structure):
    _fields_ = [("SysTimerCountOfFirstDataPoint",c_uint64),     #当前数据包第一个数据点对应的系统时间戳。
                ("InPacketTriggeredDataSize",c_uint16),         #当前数据包中有效触发数据的字节数。
                ("InPacketTriggerEdges",c_uint16),              #当前数据包中包含的触发沿数量。
                ("StartDataIndexOfTriggerEdges",c_uint32 * 25), #当前数据包中触发沿对应的数据位置。
                ("SysTimerCountOfEdges",c_uint64 * 25),         #当前数据包中触发沿的系统时间戳。
                ("EdgeType",c_int8 * 25)]                       #当前数据包中各触发沿的极性。

class IQStream_TypeDef(Structure):
    _fields_ = [("AlternIQStream",POINTER(c_void_p)),           #交织的IQ时域数据；单通道数据格式可为i8、i16或i32 。
                ("IQS_ScaleToV",c_float),                       #将整数类型转换为绝对电压（V）的系数。
                ("MaxPower_dBm",c_float),                       #当前数据包中的最大功率值（单位：dBm）。
                ("MaxIndex",c_uint32),                          #当前数据包中最大功率值的索引位置。
                ("IQS_Profile",IQS_Profile_TypeDef),            #IQ流配置概要信息，通常通过IQS_Configuration函数（IQS_ProfileOut）更新。
                ("IQS_StreamInfo", IQS_StreamInfo_TypeDef),     #IQ流格式信息，通常通过IQS_Configuration函数更新。
                ("IQS_TriggerInfo", TriggerInfo_TypeDef),       #与IQ流相关的触发信息，通常通过IQS_GetIQStream更新。
                ("DeviceInfo", DeviceInfo_TypeDef),             #与IQ流相关的设备信息，通常通过IQS_GetIQStream更新。
                ("DeviceState", DeviceState_TypeDef)]           #与IQ流相关的设备状态信息，通常通过IQS_GetIQStream更新。


#检波模式结构（检波器）
class DET_Profile_TypeDef(Structure):
    _fields_ = [("CenterFreq_Hz",c_double),                                 #中心频率
                ("RefLevel_dBm",c_double),                                  #参考电平
                ("DecimateFactor",c_uint32),                                #时间抽取因子
                ("RxPort",RxPort_TypeDef),                                  #射频输入端口
                ("BusTimeout_ms",c_uint32),                                 #传输超时
                ("TriggerSource",DET_TriggerSource_TypeDef),                #输入触发源
                ("TriggerEdge",TriggerEdge_TypeDef),                        #输入触发边沿
                ("TriggerMode",TriggerMode_TypeDef),                        #输入触发模式	
                ("TriggerLength",c_uint64),                                 #输入触发后采样点数。仅在FixedPoints模式下生效
                ("TriggerOutMode",TriggerOutMode_TypeDef),                  #触发输出模式       
                ("TriggerOutPulsePolarity",TriggerOutPulsePolarity_TypeDef),#触发输出脉冲极性
                ("TriggerLevel_dBm",c_double),                              #电平触发阈值
                ("TriggerLevel_SafeTime",c_double),                         #电平触发防抖保护时间，单位秒
                ("TriggerDelay",c_double),                                  #触发延迟，单位秒
                ("PreTriggerTime",c_double),                                #预触发时间，单位秒
                ("TriggerTimerSync",TriggerTimerSync_TypeDef),              #定时与外触发边沿同步选项。
                ("TriggerTimer_Period",c_double),                           #定时触发的触发周期，单位秒。	
                ("EnableReTrigger",c_uint8),                                #使能设备捕获触发后多次响应。仅在FixedPoints模式下可用
                ("ReTrigger_Period",c_double),                              #触发设备多次响应的时间间隔。在定时器触发模式下也用作触发周期（单位：秒）
                ("ReTrigger_Count",c_uint16),                               #一次触发后，除触发响应外还需进行若干次响应。
                ("Detector",Detector_TypeDef),                              #检波
                ("DetectRatio",c_uint16),                                   #检波率：检测器对功率轨迹进行检测，每个原始数据点对应1个输出轨迹点。
                ("GainStrategy",GainStrategy_TypeDef),                      #增益策略。
                ("Preamplifier",PreamplifierState_TypeDef),                 #前置放大器动作
                ("AnalogIFBWGrade",c_uint8),                                #中频带宽档位。
                ("IFGainGrade",c_uint8),                                    #中频增益档位。
                ("EnableDebugMode",c_uint8),                                #调试模式：不建议用于高级应用，默认值为0。
                ("ReferenceClockSource",ReferenceClockSource_TypeDef),      #参考时钟源。    
                ("ReferenceClockFrequency",c_double),                       #参考时钟频率。
                ("EnableReferenceClockOut",c_uint8),                        #使能参考时钟输出。
                ("SystemClockSource",SystemClockSource_TypeDef),            #系统时钟源。
                ("ExternalSystemClockFrequency",c_double),                  #外部系统时钟频率：Hz
                ("Atten",c_int8),                                           #衰减
                ("DCCancelerMode",DCCancelerMode_TypeDef),                  #适用于特定设备：直流抑制。 0：关闭DCC；1：开启高通滤波模式（抑制效果更好，但会影响DC至100 kHz范围内的信号）；2：开启手动偏置模式（需手动校准，但不会损伤低频信号）。
                ("QDCMode",QDCMode_TypeDef),                                #适用于特定设备：IQ幅相校正器。 QDCOff：关闭QDC功能；QDCManualMode：启用手动模式；QDCAutoMode：启用自动QDC模式。
                ("QDCIGain",c_float),                                       #适用于特定设备：I路归一化线性增益，1.0表示无增益，设置范围0.8～1.2。
                ("QDCQGain",c_float),                                       #适用于特定设备：Q路归一化线性增益，1.0表示无增益，设置范围0.8～1.2。
                ("QDCPhaseComp",c_float),                                   #适用于特定设备：归一化相位补偿系数，设置范围-0.2～+0.2。
                ("DCCIOffset",c_int8),                                      #适用于特定设备：I通道直流偏置，单位LSB。
                ("DCCQOffset",c_int8),                                      #适用于特定设备：Q通道直流偏置，单位LSB。
                ("LOOptimization",LOOptimization_TypeDef)]                  #本振优化

#DET 配置后返回的流信息结构（已返回）
class DET_StreamInfo_TypeDef(Structure):
    _fields_ = [("PacketCount",c_uint64),       #当前配置对应的数据包总数，仅在FixedPoints（固定点数）模式下生效。
                ("StreamSamples",c_uint64),     #在FixedPoints模式下表示当前配置对应的总采样点数；在Adaptive（自适应）模式下该值无实际意义，固定为0。
                ("StreamDataSize",c_uint64),    #在FixedPoints模式下表示当前配置对应的采样数据总字节数；在Adaptive模式下该值无实际意义，固定为0。
                ("PacketSamples",c_uint32),     #每次调用DET_GetTrace获取的数据包中的采样点数（即每个数据包包含的采样点数）。
                ("PacketDataSize",c_uint32),    #每次调用DET_GetTrace获取的有效数据字节数。
                ("TimeResolution",c_double),    #时域点分辨率。
                ("GainParameter",c_uint32)]     #与增益相关的参数，包括：Space（31～24位）、前置放大器状态（23～16位）、起始射频频段（15～8位）、终止射频频段（7～0位）。

#RTA的配置结构体(配置)
class RTA_Profile_TypeDef(Structure):
    _fields_ = [("CenterFreq_Hz",c_double),                                 #中心频率
                ("RefLevel_dBm",c_double),                                  #参考电平
                ("RBW_Hz",c_double),                                        #分辨率带宽
                ("VBW_Hz",c_double),                                        #视频带宽
                ("RBWMode",RBWMode_TypeDef),                                #RBW更新方式。手动输入、根据Span自动设置
                ("VBWMode",VBWMode_TypeDef),                                #VBW更新方式。手动输入、VBW = RBW、VBW = 0.1*RBW、 VBW = 0.01*RBW
                ("DecimateFactor",c_uint32),                                #时域数据的抽取倍数
                ("Window",Window_TypeDef),                                  #窗型
                ("SweepTimeMode",SweepTimeMode_TypeDef),                    #扫描时间模式
                ("SweepTime",c_double),                                     #当扫描时间模式指定为Manual时，该参数为绝对时间；当指定为*N时，该参数为扫描时间倍率
                ("Detector",Detector_TypeDef),                              #检波器
                ("TraceDetectMode",TraceDetectMode_TypeDef),                #迹线检波模式
                ("TraceDetectRatio",c_uint32),                              #迹线检波检波比
                ("TraceDetector",TraceDetector_TypeDef),                    #迹线检波检波器
                ("RxPort",RxPort_TypeDef),                                  #接收端口设置
                ("BusTimeout_ms",c_uint32),                                 #传输超时时间
                ("TriggerSource",RTA_TriggerSource_TypeDef),                #输入触发源
                ("TriggerEdge",TriggerEdge_TypeDef),                        #输入触发边沿
                ("TriggerMode",TriggerMode_TypeDef),                        #输入触发模式
                ("TriggerAcqTime",c_double),                                #输入触发后的采样时间，仅在FixedPoints模式下生效，单位s
                ("TriggerOutMode",TriggerOutMode_TypeDef),                  #触发输出模式
                ("TriggerOutPulsePolarity",TriggerOutPulsePolarity_TypeDef),#触发输出脉冲极性
                ("TriggerLevel_dBm",c_double),                              #电平触发：门限
                ("TriggerLevel_SafeTime",c_double),                         #电平触发：防抖安全时间，单位为s
                ("TriggerDelay",c_double),                                  #电平触发：触发延迟，单位为s
                ("PreTriggerTime",c_double),                                #电平触发：预触发时间，单位为s
                ("TriggerTimerSync",TriggerTimerSync_TypeDef),              #定时触发：与外触发边沿同步选项
                ("TriggerTimer_Period",c_double),                           #定时触发：周期
                ("EnableReTrigger",c_uint8),                                #自动重触发：使能设备在捕获到一次触发后，进行多次响应，仅可用于FixedPoint模式下
                ("ReTrigger_Period",c_double),                              #自动重触发：多次响应的时间间隔，也作为Timer触发模式下的触发周期，单位为s
                ("ReTrigger_Count",c_uint16),                               #自动重触发：每次原触发动作之后，自动重触发的执行次数
                ("GainStrategy",GainStrategy_TypeDef),                      #增益策略
                ("Preamplifier",PreamplifierState_TypeDef),                 #前置放大器动作
                ("AnalogIFBWGrade",c_uint8),                                #中频带宽档位
                ("IFGainGrade",c_uint8),                                    #中频增益档位
                ("EnableDebugMode",c_uint8),                                #调试模式，高级应用不推荐用户自行使用，默认值为 0
                ("ReferenceClockSource",ReferenceClockSource_TypeDef),      #参考时钟源
                ("ReferenceClockFrequency",c_double),                       #参考时钟频率
                ("EnableReferenceClockOut",c_uint8),                        #使能参考时钟输出
                ("SystemClockSource",SystemClockSource_TypeDef),            #系统时钟源
                ("ExternalSystemClockFrequency",c_double),                  #外部系统时钟频率，Hz
                ("Atten",c_int8),                                           #衰减
                ("DCCancelerMode",DCCancelerMode_TypeDef),                  #特定设备适用。直流抑制。0: 关闭DCC；1：开启，高通滤波器模式（更好的抑制效果，但会损伤DC至100kHz范围内的信号）；2：开启，手动偏置模式（需要人工校准，但不低频损伤信号）
                ("QDCMode",QDCMode_TypeDef),                                #特定设备适用。IQ幅相修正器。QDCOff: 关闭QDC功能；QDCManualMode：开启并使用手动模式；QDCAutoMode：开启并使用自动QDC模式
                ("QDCIGain",c_float),                                       #特定设备适用。归一化线性增益 I路，1.0 表示无增益，设置范围 0.8~1.2
                ("QDCQGain",c_float),                                       #特定设备适用。归一化线性增益 Q路，1.0 表示无增益 ，设置范围 0.8~1.2
                ("QDCPhaseComp",c_float),                                   #特定设备适用。归一化相位补偿系数，设置范围 -0.2~+0.2
                ("DCCIOffset",c_int8),                                      #特定设备适用。I通道直流偏置， LSB
                ("DCCQOffset",c_int8),                                      #特定设备适用。Q通道直流偏置， LSB
                ("LOOptimization",LOOptimization_TypeDef)]                  #本振优化

#RTA配置后返回的包信息结构体(返回)
class RTA_FrameInfo_TypeDef(Structure):
    _fields_ = [("StartFrequency_Hz",c_double),     #频谱的起始频率
                ("StopFrequency_Hz",c_double),      #频谱的终止频率
                ("POI",c_double),                   #100%截获概率下的信号最短持续时间,以s为单位。
                ("TraceTimestampStep",c_double),    #每包数据内各条Trace的时间戳步进。(包整体时间戳为TriggerInfo中的SysTimerCountOfFirstDataPoint)
                ("TimeResolution",c_double),        #每个时域数据的采样时间，也是时间戳的分辨率
                ("PacketAcqTime",c_double),         #每包数据对应的采集时间
                ("PacketCount",c_uint32),           #当前配置对应的总数据包数，仅在FixedPoints模式下生效
                ("PacketFrame",c_uint32),           #每个数据包中的有效帧数
                ("FFTSize",c_uint32),               #每帧FFT的点数
                ("FrameWidth",c_uint32),            #FFT帧截取后的点数，也是数据包中每条Trace的点数，可作为概率密度图的X轴点数(宽度)
                ("FrameHeight",c_uint32),           #FFT帧对应的频谱幅度范围，可作为概率密度图的Y轴点数(高度)
                ("PacketSamplePoints",c_uint32),    #每包数据对应的采集点数
                ("PacketValidPoints",c_uint32),     #每包数据中所含的频域有效数据点数
                ("MaxDensityValue",c_uint32),       #概率密度位图的单个位点元素值上限
                ("GainParameter",c_uint32)]         #包括Space(31~24Bit)、PreAmplifierState(23~16Bit)、StartRFBand(15~8Bit)、StopRFBand(7~0Bit)

#RTA获取后返回的绘图信息结构体
class RTA_PlotInfo_TypeDef(Structure):
    _fields_ = [("ScaleTodBm",c_float),             #由线性功率转对数功率导致的压缩。Trace的绝对功率等于 Trace[] * ScaleTodBm + OffsetTodBm(Bitmap的绝对功率轴同下)
                ("OffsetTodBm",c_float),            #相对功率转换成绝对功率的偏移。bitmap的绝对功率轴范围(Y轴)等于 FrameHeigh * ScaleTodBm + OffsetTodBm(Trace物理功率同上)
                ("SpectrumBitmapIndex",c_uint64)]   #概率密度图的获取次数，可在绘图时当做索引使用
   
#测量数据的辅助信息(返回)
class MeasAuxInfo_TypeDef(Structure):
    _fields_ = [("MaxIndex",c_uint32),              #功率最大值在当前数据包中的索引
                ("MaxPower_dBm",c_float),           #当前数据包中的功率最大值
                ("Temperature",c_int16),            #设备温度，摄氏度 = 0.01 * Temperature
                ("RFState",c_uint16),               #射频状态
                ("BBState",c_uint16),               #基带状态
                ("GainPattern",c_uint16),           #当前数据包对应频点所使用的增益控制字
                ("ConvertPattern",c_uint32),        #当前数据包频点所使用的频率变换方式
                ("SysTimeStamp",c_double),          #当前数据包所对应的系统时间戳，单位s
                ("AbsoluteTimeStamp",c_double),     #当前数据包对应的绝对时间戳
                ("Latitude",c_float),               #当前数据包对应的纬度坐标,北纬为正数，南纬为负数，以此区分南北纬
                ("Longitude",c_float)]              #当前数据包对应的经度坐标,东经为正数，西经为负数，以此区分东西经
    
DET_TriggerInfo_TypeDef = TriggerInfo_TypeDef
RTA_TriggerInfo_TypeDef = TriggerInfo_TypeDef

class DSP_FFT_TypeDef(Structure):
    _fields_ = [("FFTSize",c_uint32),              #FFT分析点数
                ("SamplePts",c_uint32),           #有效采样点数
                ("WindowType",Window_TypeDef),            #窗型
                ("TraceDetector",TraceDetector_TypeDef),               #视频检波方式
                ("DetectionRatio",c_uint32),               #迹线检波比
                ("Intercept",c_float),           #输出频谱截取，例如 Intercept = 0.8 则表示输出 80%的频谱结果
                ("Calibration",c_bool)]        #是否进行校准

class DSP_DDC_TypeDef(Structure):
    _fields_ = [("DDCOffsetFrequency",c_double), #复混频的频率偏移值
                ("SampleRate",c_double),         #复混频的采样率
                ("DecimateFactor",c_float),      #重采样抽取倍数,范围:1 ~ 2^16
                ("SamplePoints",c_uint64),       #采样点数     
                ]

class ASG_Profile_TypeDef(Structure):
    _fields_ = [("CenterFreq_Hz",c_double),         #当前中心频率，单位为Hz，在信号源工作在SIG_Fixed模式下生效；输入范围1M-1GHz;步进1Hz
                ("Level_dBm",c_double),             #当前功率，单位为dBm，在信号源工作在SIG_Fixed模式下生效；输入范围-127--5dBm；步进0.25dB
                ("StartFreq_Hz",c_double),          #频率扫描模式下的起始频率，单位为Hz，在信号源工作在SIG_FreqSweep_*模式下生效；输入范围1M-1GHz;步进1Hz
                ("StopFreq_Hz", c_double),          #频率扫描模式下的终止频率，单位为Hz，在信号源工作在SIG_FreqSweep_*模式下生效；输入范围1M-1GHz;步进1Hz
                ("StepFreq_Hz", c_double),          #频率扫描模式下的步进频率，单位为Hz，在信号源工作在SIG_FreqSweep_*模式下生效；输入范围1M-1GHz;步进1Hz
                ("StartLevel_dBm", c_double),       #功率扫描模式下的起始功率，单位为dBm
                ("StopLevel_dBm", c_double),        #功率扫描模式下的终止功率，单位为dBm
                ("StepLevel_dBm", c_double),        #功率扫描模式下的步进功率，单位为dBm
                ("DwellTime_s", c_double),          #频率扫描模式 或 功率扫描模式 下，单位为s，当触发模式为BUS时，扫描驻留时间，单位为s，在信号源工作在*Sweep*模式下生效；输入范围0-1000000；步进1
                ("ReferenceClockFrequency", c_double),      #指定参考频率：对内参考和外参考均生效

                ("ReferenceClockSource",ReferenceClockSource_TypeDef),              #选择参考时钟的输入源：内参考或外参考
                ("Port",ASG_Port_TypeDef),                                          #信号源输出端口
                ("Mode",ASG_Mode_TypeDef),                                          #关闭、点频、频率扫描（外触发，同步至接收）、功率扫描（外触发，同步至接收）
                ("TriggerSource",ASG_TriggerSource_TypeDef),                        #信号源触发输入模式
                ("TriggerInMode",ASG_TriggerInMode_TypeDef),                        #信号源的触发模式
                ("TriggerOutMode",ASG_TriggerOutMode_TypeDef)]                      #信号源的触发模式
    
class GNSSInfo_TypeDef(Structure):
    _fields_ = [
        ("latitude",c_float),                   # 经度
        ("longitude",c_float),                   # 纬度
        ("altitude",c_int16),                   # 海拔
        ("SatsNum",c_uint8),                   # 当前使用卫星数量
        ("GNSS_LockState",c_uint8),              # GNSS锁定状态
        ("DOCXO_LockState",c_uint8),              # DOCXO锁定状态
        ("DOCXO_WorkMode",DOCXOWorkMode_TypeDef),   # DOCXO工作状态
        ("GNSSAntennaState",GNSSAntennaState_TypeDef),# 天线状态
        ("hour",c_int16),
        ("minute",c_int16),
        ("second",c_int16),
        ("Year",c_int16),
        ("month",c_int16),
        ("day",c_int16),]

class GNSS_SNR_TypeDef(Structure):
    _fields_ = [
        ("Max_SatxC_No", c_uint8),  # 最大信噪比
        ("Min_SatxC_No", c_uint8),  # 最小信噪比
        ("Avg_SatxC_No", c_uint8),  # 平均信噪比
    ]

class GNSS_SatDate_TypeDef(Structure):
    _fields_ = [
        ("SatsNum_All", c_uint8),          # 当前范围可视卫星
        ("SatsNum_Use", c_uint8),          # 用于定位的卫星数量
        ("GNSS_SNR_UsePos", GNSS_SNR_TypeDef),     # 用于定位的卫星 SNR
        ("GNSS_SNR_NotUsePos", GNSS_SNR_TypeDef),]
    
class ASG_Info_TypeDef(Structure):
    _fields_ = [("SweepPoints",c_uint32)]         #当前中心频率，单位为Hz，在信号源工作在SIG_Fixed模式下生效；输入范围1M-1GHz;步进1Hz

dll.Get_APIVersion.argtypes = []
dll.Get_APIVersion.restype = c_int

#dev接口，打开设备，在对设备操作前必须调用此函数以获取设备资源
dll.Device_Open.argtypes = [POINTER(c_void_p),c_int,POINTER(BootProfile_TypeDef),POINTER(BootInfo_TypeDef)]
dll.Device_Open.restype = c_int

#dev接口，关闭设备，在对设备操作完成后必须调用此函数以释放设备资源
dll.Device_Close.argtypes = [POINTER(c_void_p)]
dll.Device_Close.restype = c_int

#dev接口，获取设备信息，包括设备序列号及软硬件版本等相关信息，非实时方式，不打断数据获取，但信息只在获取数据包后更新
dll.Device_QueryDeviceInfo.argtypes = [POINTER(c_void_p),POINTER(DeviceInfo_TypeDef)]
dll.Device_QueryDeviceInfo.restype = c_int

#dev接口，获取设备状态，包括设备温度、硬件工作状态、地理时间信息（需要选件支持）等，非实时方式，不打断数据获取，但信息只在获取数据包后更新
dll.Device_QueryDeviceState.argtypes = [POINTER(c_void_p),POINTER(DeviceState_TypeDef)]
dll.Device_QueryDeviceState.restype = c_int

# dev接口，设置GNSS天线状态（需要选件支持）
dll.Device_SetGNSSAntennaState.argtypes = [POINTER(c_void_p), GNSSAntennaState_TypeDef]
dll.Device_SetGNSSAntennaState.restype = c_int

# dev接口，获取GNSS天线状态（需要选件支持），非实时方式
dll.Device_GetGNSSAntennaState.argtypes = [POINTER(c_void_p), POINTER(GNSSAntennaState_TypeDef)]
dll.Device_GetGNSSAntennaState.restype = c_int

# dev接口，获取GNSS天线状态（需要选件支持），实时方式
dll.Device_GetGNSSAntennaState_Realtime.argtypes = [POINTER(c_void_p), POINTER(GNSSAntennaState_TypeDef)]
dll.Device_GetGNSSAntennaState_Realtime.restype = c_int

# dev接口，解析GNSS时间日期信息（需要选件支持）
dll.Device_AnysisGNSSTime.argtypes = [c_double, POINTER(c_int16), POINTER(c_int16), POINTER(c_int16),POINTER(c_int16), POINTER(c_int16), POINTER(c_int16)]
dll.Device_AnysisGNSSTime.restype = c_int

# dev接口，获取GNSS海拔（需要选件支持），非实时方式
dll.Device_GetGNSSAltitude.argtypes = [POINTER(c_void_p), POINTER(c_int16)]
dll.Device_GetGNSSAltitude.restype = c_int

# dev接口，获取GNSS海拔（需要选件支持），实时方式
dll.Device_GetGNSSAltitude_Realtime.argtypes = [POINTER(c_void_p), POINTER(c_int16)]
dll.Device_GetGNSSAltitude_Realtime.restype = c_int

# dev接口，设置GNSS中DOCXO工作状态（需要选件支持）
dll.Device_SetDOCXOWorkMode.argtypes = [POINTER(c_void_p), DOCXOWorkMode_TypeDef]
dll.Device_SetDOCXOWorkMode.restype = c_int

# dev接口，获取GNSS中DOCXO工作状态（需要选件支持），实时方式
dll.Device_GetDOCXOWorkMode_Realtime.argtypes = [POINTER(c_void_p), POINTER(DOCXOWorkMode_TypeDef)]
dll.Device_GetDOCXOWorkMode_Realtime.restype = c_int

# dev接口，获取GNSS中DOCXO工作状态（需要选件支持），非实时方式
dll.Device_GetDOCXOWorkMode.argtypes = [POINTER(c_void_p), POINTER(DOCXOWorkMode_TypeDef)]
dll.Device_GetDOCXOWorkMode.restype = c_int

# dev接口，获取GNSS设备状态（需要选件支持），非实时方式
dll.Device_GetGNSSInfo.argtypes = [POINTER(c_void_p), POINTER(GNSSInfo_TypeDef)]
dll.Device_GetGNSSInfo.restype = c_int

# dev接口，获取GNSS设备状态（需要选件支持），实时方式
dll.Device_GetGNSSInfo_Realtime.argtypes = [POINTER(c_void_p), POINTER(GNSSInfo_TypeDef)]
dll.Device_GetGNSSInfo_Realtime.restype = c_int

#dev接口，获取GNSS信噪比（需要选件支持），非实时方式，不打断数据获取，但信息只在获取数据包后更新
dll.Device_GetGNSS_SatDate.argtypes = [POINTER(c_void_p), POINTER(GNSS_SatDate_TypeDef)]
dll.Device_GetGNSS_SatDate.restype  = c_int

#dev接口，获取GNSS信噪比（需要选件支持），实时获取，短时间内会占用数据通道
dll.Device_GetGNSS_SatDate_Realtime.argtypes = [POINTER(c_void_p), POINTER(GNSS_SatDate_TypeDef)]
dll.Device_GetGNSS_SatDate_Realtime.restype  = c_int

#SWP模式，配置SWP_Profile 为默认值
dll.SWP_ProfileDeInit.argtypes = [POINTER(c_void_p),POINTER(SWP_Profile_TypeDef)]
dll.SWP_ProfileDeInit.restype = c_int

#SWP模式，配置SWP模式相关参数
dll.SWP_Configuration.argtypes = [POINTER(c_void_p),POINTER(SWP_Profile_TypeDef),POINTER(SWP_Profile_TypeDef),POINTER(SWP_TraceInfo_TypeDef)]
dll.SWP_Configuration.restype = c_int

#SWP模式，获取SWP模式下部分扫描的频谱数据
dll.SWP_GetPartialSweep.argtypes = [POINTER(c_void_p),POINTER(c_double),POINTER(c_float),POINTER(c_int),POINTER(c_int),POINTER(MeasAuxInfo_TypeDef)]
dll.SWP_GetPartialSweep.restype = c_int

#SWP模式，获取SWP模式下完整刷新的频谱数据
dll.SWP_GetFullSweep.argtypes = [POINTER(c_void_p),POINTER(c_double),POINTER(c_float),POINTER(MeasAuxInfo_TypeDef)]
dll.SWP_GetFullSweep.restype = c_int

#IQS模式，配置IQS_Profile 为默认值
dll.IQS_ProfileDeInit.argtypes = [POINTER(c_void_p),POINTER(IQS_Profile_TypeDef)]
dll.IQS_ProfileDeInit.restype = c_int

#IQS模式，配置IQS模式相关参数
dll.IQS_Configuration.argtypes = [POINTER(c_void_p),POINTER(IQS_Profile_TypeDef),POINTER(IQS_Profile_TypeDef),POINTER(IQS_StreamInfo_TypeDef)]
dll.IQS_Configuration.restype = c_int

#IQS模式，发起总线触发
dll.IQS_BusTriggerStart.argtypes = [POINTER(c_void_p)]
dll.IQS_BusTriggerStart.restype = c_int

#IQS模式，终止总线触发
dll.IQS_BusTriggerStop.argtypes = [POINTER(c_void_p)]
dll.IQS_BusTriggerStop.restype =c_int

#IQS模式，获取IQS模式下交织的IQ数据流，并得到整型至绝对幅度（V单位）的比例因子 及 触发相关的信息
dll.IQS_GetIQStream.argtypes = [POINTER(c_void_p),POINTER(c_int16_p),POINTER(c_float),POINTER(TriggerInfo_TypeDef),POINTER(MeasAuxInfo_TypeDef)]
dll.IQS_GetIQStream.restype = c_int
dll.IQS_GetIQStream_PM1.argtypes = [POINTER(c_void_p),POINTER(IQStream_TypeDef)]
dll.IQS_GetIQStream_PM1.restype = c_int

#DET模式，配置DET_Profile 为默认值
dll.DET_ProfileDeInit.argtypes = [POINTER(c_void_p),POINTER(DET_Profile_TypeDef)]
dll.DET_ProfileDeInit.restype = c_int

#DET模式，配置DET模式相关参数
dll.DET_Configuration.argtypes = [POINTER(c_void_p),POINTER(DET_Profile_TypeDef),POINTER(DET_Profile_TypeDef),POINTER(DET_StreamInfo_TypeDef)]
dll.DET_Configuration.restype = c_int

#DET模式，发起总线触发
dll.DET_BusTriggerStart.argtypes = [POINTER(c_void_p)]
dll.DET_BusTriggerStart.restype = c_int

#DET模式，终止总线触发
dll.DET_BusTriggerStop.argtypes = [POINTER(c_void_p)]
dll.DET_BusTriggerStop.restype = c_int

#DET模式，获取DET模式下的检波数据，并得到整型至绝对幅度（V单位）的比例因子 及 触发相关的信息，NormalizedPowerStream为 I方+Q方开根号
dll.DET_GetPowerStream.argtypes = [POINTER(c_void_p),POINTER(c_float),POINTER(c_float),POINTER(DET_TriggerInfo_TypeDef),POINTER(MeasAuxInfo_TypeDef)]
dll.DET_GetPowerStream.restype = c_int

#RTA模式，配置RTA_Profile 为默认值
dll.RTA_ProfileDeInit.argtypes = [POINTER(c_void_p),POINTER(RTA_Profile_TypeDef)]
dll.RTA_ProfileDeInit.restype = c_int

#RTA模式，配置RTA模式相关参数
dll.RTA_Configuration.argtypes = [POINTER(c_void_p),POINTER(RTA_Profile_TypeDef),POINTER(RTA_Profile_TypeDef),POINTER(RTA_FrameInfo_TypeDef)]
dll.RTA_Configuration.restype = c_int

#RTA模式，发起总线触发
dll.RTA_BusTriggerStart.argtypes = [POINTER(c_void_p)]
dll.RTA_BusTriggerStart.restype = c_int

#RTA模式，终止总线触发
dll.RTA_BusTriggerStop.argtypes = [POINTER(c_void_p)]
dll.RTA_BusTriggerStop.restype = c_int

#RTA模式，获取RTA模式下的实时频谱数据 及 触发相关的信息
dll.RTA_GetRealTimeSpectrum.argtypes = [POINTER(c_void_p),POINTER(c_uint8),POINTER(c_uint16),POINTER(RTA_PlotInfo_TypeDef),POINTER(RTA_TriggerInfo_TypeDef),POINTER(MeasAuxInfo_TypeDef)]
dll.RTA_GetRealTimeSpectrum.restype = c_int

#DSP辅助函数
#DSP辅助函数，启动DSP功能
dll.DSP_Open.argtypes = [POINTER(c_void_p)]
dll.DSP_Open.restype = c_int

dll.DSP_Close.argtypes = [POINTER(c_void_p)]
dll.DSP_Close.restype = c_int

#DSP辅助函数，初始化FFT参数
dll.DSP_FFT_DeInit.argtypes = [POINTER(DSP_FFT_TypeDef)]
dll.DSP_FFT_DeInit.restype = c_int

#DSP辅助函数，初始化DDC配置参数
dll.DSP_DDC_DeInit.argtypes = [POINTER(DSP_DDC_TypeDef)]
dll.DSP_DDC_DeInit.restype = c_int

#DSP辅助函数，配置DDC
dll.DSP_DDC_Configuration.argtypes = [POINTER(c_void_p),POINTER(DSP_DDC_TypeDef),POINTER(DSP_DDC_TypeDef)]
dll.DSP_DDC_Configuration.restype = c_int

#DSP辅助函数，执行DDC
dll.DSP_DDC_Execute.argtypes = [POINTER(c_void_p),POINTER(IQStream_TypeDef),POINTER(IQStream_TypeDef)]
dll.DSP_DDC_Execute.restype = c_int

#DSP辅助函数，重置DDC中的缓存
dll.DSP_DDC_Reset.argtypes = [POINTER(c_void_p)]
dll.DSP_DDC_Reset.restype = c_int

#DSP辅助函数，配置FFT参数
dll.DSP_FFT_Configuration.argtypes = [POINTER(c_void_p),POINTER(DSP_FFT_TypeDef),POINTER(DSP_FFT_TypeDef),POINTER(c_uint32), POINTER(c_double)]
dll.DSP_FFT_Configuration.restype = c_int

#DSP辅助函数，将IQ数据转换成频谱数据
dll.DSP_FFT_IQSToSpectrum.argtypes = [POINTER(c_void_p),POINTER(IQStream_TypeDef),POINTER(c_double),POINTER(c_float)]
dll.DSP_FFT_IQSToSpectrum.restype = c_int

#模拟信号源，配置Profile 为默认值
dll.ASG_ProfileDeInit.argtypes = [POINTER(c_void_p),POINTER(ASG_Profile_TypeDef)]
dll.ASG_ProfileDeInit.restype = c_int

#模拟信号源，工作状态配置
dll.ASG_Configuration.argtypes = [POINTER(c_void_p),POINTER(ASG_Profile_TypeDef),POINTER(ASG_Profile_TypeDef),POINTER(ASG_Info_TypeDef)]
dll.ASG_Configuration.restype = c_int


