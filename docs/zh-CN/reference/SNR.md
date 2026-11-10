# 原生 IQ 与协议测量

[文档](../INDEX.md) · [English](../../en/reference/SNR.md)

RF SNR 默认关闭。需要时，在信道的模式设置中勾选**测量 RF SNR**并应用设置，才会开始计算并显示实时卡片。关闭后释放分析器并停止 RF 测量事件；解调、协议 SNR 和音频继续运行。处理器 API 字段为 `rf_snr`（布尔值，默认 `false`）；关闭时不提供 `observed_receiver.rf_measurement`。

## 定义与接口

原生接收器通过 SDE2 发送 `rf-measurement`，同时在 `processing_runtime.observed_receiver` 中提供最新 `rf_measurement`。测量继承事件封装的接收器 UID、服务 ID、细信道 ID、采集 epoch、处理代次和样本时钟。历史 FT8 解码记录描述原来的发射突发，不表示当前信道读数。

| 指标 | 定义与单位 | 带宽与测量位置 | 方法与有效条件 |
|---|---|---|---|
| RF SNR | `10 log10((Pchannel − N0 B)/(N0 B))`，dB | `band_low_hz` 到 `band_high_hz`，相对调谐频率；在解调、AGC、静噪、降噪和 EQ 之前 | 固定 Hann/Welch 分析；**有条件估计**，假定稀疏占用及可叠加的平坦背景。分子包括所有带内信号及其他电台；背景可能包含无法分离的干扰。 |
| FT8 SNR | 解码音调功率扣除匹配滤波器背景，再除以 `N0 × 2500 Hz`，dB | **2500 Hz**；抵消前原始 IQ，每个完整观测码元的中间 75% | 六个固定 ±5/7/9 bin 的 Hann 参考，以中位数初始化并进行指数分布上尾校正；数值与质量分开，不限幅、不按测试范围置空。 |
| FAC 加权 MER | `−10 log10(Σ |H|² |Y/H − Dnearest|² / Σ |H|²)`，dB | 均衡后的 FAC QPSK 单元，平均一帧；附带的 DRM 标称占用带宽是上下文，不是 MER 的积分带宽 | 包含噪声、干扰、失真和均衡误差；低 MER 存在最近点判决偏差。不是 RF SNR。 |
| FAC 判决 SNR | FAC MER 加 `10 log10(avg_cell_power × Ncar × Δf/Bnominal)`，dB | DRM30 占用带宽；DRM+ 为 **100 kHz**，不能套用 DRM30 占用码零的 4.5 kHz | 仅在**平坦白噪声信道**下解释为噪声比。MER 低于 10 dB、尚未 FAC 锁定、当前 FAC 失败或误差低于数值分辨率时不可用；每帧均清零累计，包括零误差帧。 |
| WFM 导频对比度 | 平滑 `20 log10(A19k/A21k)`，dB | 鉴频复合信号经抗混叠及抽取后；单极点双边 ENBW 为 `Fs α/(2−α)`，50 Hz 极点对应约 157 Hz | 平滑时间常数 2 s；是导频 SNR 的诊断近似，没有进行功率噪声扣除，不表示 RF 或节目音频 SNR。该统计不控制监听增益或降噪。 |
| 外部报告 | 原始数值、单位和 provider | 由 provider 定义 | 算法和原始输出由 provider 定义。GPS 逐卫星 C/N₀ 保持 **dB-Hz**。 |

原生测量携带 `metric_type`、`unit`、`reference_bandwidth_hz`、`location`、`method`、`averaging_seconds`、`state` 和 `reason`。RF 还提供观察时长、FFT 间距、窗 ENBW、平坦通带、ADC 满刻度平方单位的线性功率、可用参考比例、统计不确定度和最长有效年龄。不可用值为 JSON `null`；保留负数 SNR。旧 DSP/WASM 数字接口用 NaN 表示不可用 FT8 值，原生 JSON 将其序列化为 null。

没有干净参考的任意语音、音乐或图像，不宣称测得精确音频 SNR。已知测试音 SNR/SINAD/THD 仍是独立台架指标。Wiener、DeepFilterNet 内部估计及 DRM Wiener 正则化不作为 RF 读数或实测音质改善量。

## FT8 数值与质量

`snrDb` 与 `measurement.value` 是同一个有限估计值或 null。有限值标记为
`value_kind: estimate`，本实现不输出限幅值或边界值。有限估计使用 `state: conditional`，不可用值使用 `state: unavailable`。
`quality: good/degraded/unavailable` 和 `quality_reasons` 为可选元数据。
测试覆盖范围和 −25…+10 dB 区间不决定数值显示许可；参考相差超过 6 dB 表示质量受限。

使用经过指数分布次序统计校正的中位数初始化有界噪声拟合。每轮剔除超过当前
噪声尺度四倍的参考功率，并按 `E[X | X < 4μ]/μ = 1 − 4/(exp(4) − 1)`
补偿被剔除的白噪声上尾。两侧参考共同参与估计，不选择最安静一侧或最低噪声点。
两侧不平衡、参考重尾、通带边缘、部分突发、重复 IQ 峰值和统计不确定度独立报告。
公共幅度缩放不会直接判为削顶。部分突发只估计已完整观测的码元，不能解释为完整
发射突发的平均值。统计不确定度是近似值，不包含衰落、模型误差和干扰偏差。

测量失败不影响已解码消息。仅在信号或噪声无法分辨、观测不足、参数或码字无效、
IQ 非有限等情况返回 null，并附具体原因。质量受限但可用的数值继续显示，以兼容
深浅主题的较淡颜色及点状下划线标记，悬停查看原因；“—”也提供原因说明。
CSV 追加数值类型、质量、定义及完整测量 JSON，JSONL 保留相同对象；兼容旧数字报告。
浮窗显示缓冲、尝试和解码计数，不显示线性 IQ RMS 百分比。
解码行用于查看消息，不触发点击调谐。接近零的负估计取整显示为 `0`，不显示 `−0`；悬停显示
两位小数的估计值，导出继续保留原始未取整数值。

这是局部背景估计，不是独立发射机功率的精密测量。同频信号不可分离；多数参考
持续受污染、背景急剧弯曲以及接收滤波都可能造成偏差。质量提示不是误差上界。
参见[固定提交的官方源码核对](FT8_SNR_REVIEW.md)、
[预先确定的验收条件](../../ft8-snr-acceptance.md)及
[修改前后验证记录](../../../tools/bench/ft8_snr_validation_v2.json)。

## RF 分析与限制

复数双边 PSD 为 `|FFT(xw)|²/(Fs Σw²)`。边界 bin 按实际覆盖比例积分，使用真实 IQ 采样率，不使用取整音频时钟。周期 Hann 的 ENBW 为 `1.5 Fs/N`。该归一化守恒复数功率，不重复除以相干增益或 ENBW。

上游 FIR 的已知平坦通带来自实际基带计划。噪声参考避开四个边缘 bin 及 Hann 泄漏邻域；排除强频谱分量后，至少需要 64 个 bin、60% 可用参考，并要求八个频率区间都有参考且均值相差不超过 1.5 dB。不读取受抑制的阻带作为底噪，也不逆补偿深度衰减。真实信道 FIR 的合成测试验证滤波后的功率；这不是对任意射频硬件频响的校准。

纯噪声、低于三倍标准差分辨门限的信号、参考不足、宽带占满、背景或频响不平坦、严重泄漏及过载返回不可用。**仅凭 IQ 不能识别与噪声无法区分的宽带干扰。** 即使读数有效，也属于有条件估计。WFM、DRM 或任何占满平坦通带的信号可能没有可识别背景，因此没有 RF SNR。分析器与解码及录制共用信道化 IQ，不要求独立宽带采集。

校验覆盖白色高斯噪声中孤立或稀疏信号的输入 RF SNR **−10…35 dB**。考虑 1 dB 误差余量，边界读数允许略微超出；−11…36 dB 以外返回不可用。`uncertainty_db` 是近似一倍标准差的采样不确定度，不是硬件校准结果或保证的置信区间。固定测试是可重复校验，不是完整的统计认证。

每信道使用 512～4096 点、32 个不重叠窗，最多每秒 64 次 FFT，复用 FFT 存储，不增加队列。窄信道平均时间更长：2 ksample/s、250 Hz 带宽时观察 65.536 s；390625 sample/s、100 kHz 时约 0.5 s。有效平均时长与观察时长均会报告。RF 超过两倍观察预算加 0.5 s 后失效，包括浏览器数据流停止的情况。换频、带宽/采样率/平坦通带变化、采集重启和样本中断会清除不兼容历史；本地音量不清除 RF 历史；硬件增益重配置建立新的采集身份。拒绝信道 IQ 中可见的削顶，但不能证明上游被滤波掩盖的削顶不存在。

## 验证

```sh
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib rf_ -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib snr_measurement_tests -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test ft8_snr_robust -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test ft8_gfsk --test ft8_snr_sweep --test ft8_osd --test ft8_reference --test ft8_real_slot --test ft8_subtract
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib decision_metric_validation -- --nocapture
python3 tools/bench/compare_ft8_snr.py --reference-bin /tmp/wsjtx/usr/bin --output /tmp/ft8-snr
```

参考工具需要 NumPy 及独立的 WSJT-X `jt9`、`ft8sim`。Debian WSJT-X
**2.7.0+repack-1** 提供可复现的二进制参考。`ft8sim` 合成 GFSK，以已知有效
突发功率确定固定种子 AWGN 的方差，两个解码器接收完全相同的量化样本。
工具记录程序/输入哈希、参考输出和原生 CSV。灵敏度对比要求实际恢复载荷；
已知码字测量只能验证校准。`ft8_gfsk` 使用独立 BT=2 合成，覆盖不同起始时刻、
载频偏移、纯噪声误报与解码时间预算。

[FT8 验证记录](../../../tools/bench/ft8_snr_validation_v2.json)包含全部数值样例、
误差统计、质量标记、解码率和性能结果。[验收规范](../../ft8-snr-acceptance.md)
定义数值及资源门槛。`tools/bench/ft8_snr_cost.py` 对预先编译的程序执行七轮
交替串行测量，计时区间排除编译和其他测试；进程 CPU 包含相同的输入准备及测试框架。

[RF/DRM 验证数据](../../../tools/bench/snr_validation.json)包含采样率、带宽、
注入功率、FIR/衰落样例和判决偏差。RF 检查覆盖分块无关性、纯噪声、
增益/AGC/静噪/降噪无关性、信道隔离、调谐失效、真实 FIR 响应、频偏、衰落及
过期/过载状态。显示 FFT、频谱/瀑布缩放和本地音频控制不属于测量器配置。
自然接收不提供绝对 SNR 真值。

参考：[2500 Hz 约定](https://wsjt.sourceforge.io/wsjtx-main_en.html)、
[固定提交的 WSJT-X/JTDX 源码比较](FT8_SNR_REVIEW.md)、
[ETSI FAC MER/SNR 定义](https://www.etsi.org/deliver/etsi_TS/102300_102399/102349/04.02.01_60/ts_102349v040201p.pdf)。

## WFM 音频与连续性

IQ 和 IQ/2 重采样器保留有界的未配对尾部，只有对应样本进入立体声矩阵和联动 AGC；
换频清除尾部状态。鉴频复合信号在抽取前经过 33 抽头半带 FIR。整数单声道延迟与
居中的 33 抽头分数立体声差分延迟对齐相位及高频增益。在 325520.8333 → 48000
sample/s 下，附加呈现延迟为 19 帧（0.396 ms），通过 AUD2 报告。该路径使用固定
FIR/延迟状态和复用 f64 分块缓冲，不增加 IQ 队列。

连续性检查覆盖分数输入采样率、44.1/48/192 kHz 输出、1/17/9765/11165/12180
样本分块及换频。带外测试音要求在 300.480769/325.520833/488.28125 ksample/s
及 44.1/48 kHz 输出下至少抑制 50 dB。这些检查量化合成混叠抑制和波形一致性，
不测量节目音频 SNR 或主观接收质量。结构化波形数据见
[音频诊断记录](../../../tools/bench/fm_audio_diagnostic.json)。

```sh
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib analog::
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib reused_worker_history_matches_serial_bits_at_partial_boundaries
```
