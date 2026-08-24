# HAROGIC FAQ V2.2 要点 (项目设计相关)

## API 调用约束
1. **同一时刻仅允许一条函数调用** → 单进程串行模型(本项目核心架构)
2. 单台设备不支持 API 与软件同时运行 (SAStudio4 占用时 Device_Open 返回 -1)
3. 配置类函数可多次调用; 未配置参数取初始化默认值
4. 持续采集需循环调用 Get 函数

## 频谱/SWP
- SWP_GetFullSweep 获取范围比下发参数宽 → 必须 DSP_InterceptSpectrum 裁剪
- 迹线点数是用户期望值, 设备按内部策略调整真实取值
- RBW 由采样率/窗因子/抽取倍数/采样点数计算
- 底噪: 减小参考电平 + RBW

## 参考时钟
- ReferenceClockSource: 0=Int, 1=Ext(失锁自动回退内部), 2=Int+(DOCXO), 3=ExtForce(失锁不回退)
- 外部参考频率: SAN-90 外部输入为 10MHz (SCIPI 示例 ROSC:EXT:FREQ 10MHz)
- EnableReferenceClockOut: 0 不输出, 1 输出参考时钟; 仅 9G 及以上型号支持
- SystemClockSource 切外部是**危险配置**(厂商指导下使用), 会致设备挂死
- 参考时钟校准: Device_CalibrateRefClock(GNSS 1PPS, TriggerCount≥30, 需 1PPS 实际信号)

## 相噪(PNM)
- 100Hz 与 10MHz 边界频偏结果不准; 建议 SWP 模式扫宽=2×最大频偏
- 相噪最小输入功率典型值 -50dBm
- 增量采集: FrameUpdateCounts 各段帧数递增, 每次 Get 返回部分累积(实时显示机制)

## 其他
- 中频增益 1/4 档部分频点 ~1dB 幅度差异
- 杂散抑制算法仅 SWP 模式有效
- SAN 系列: SAN-45 9kHz-4.5GHz / SAN-60 9kHz-6GHz / SAN-90 9kHz-9GHz, 功能相同指标不同
