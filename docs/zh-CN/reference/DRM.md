# DRM 开发参考

[文档目录](../INDEX.md)

## 台架流程

使用可重复生成的本地信号采集 DRM 基带，并对比解码器结果。夹具来源、清单、格式与重新生成方法见 [DRM 夹具 README](../../../tests/fixtures/drm/README.md)。

### 配置

[电台配置](../../../tools/bench/drm_bench/station_iq.toml) 当前生成 mode B、SO3（10 kHz）、64-QAM MSC、16-QAM SDC、长交织、保护 A/B 0/1，以及标为 `SAN90 DRM BENCH` 的单声道 **xHE-AAC** 1 kHz 单音。IQ 输出为 int16 立体声（左声道为 I），电平 -15 dBFS；单音电平 -12 dBFS。已提交的真实抓取早于这份配置，承载的是 HE-AAC（12 kHz 核心 + SBR，24 kHz PCM），不是 xHE-AAC。

以下是一组台架配置示例，实际输入电平和采集几何应以本次设备回读为准：

- 发射本振：400 MHz；DRM 偏移：+100 kHz，以避开本振泄漏。
- 分析仪：SDR 模式，中心与收听频率 400.1 MHz，decimate 256（约 195 kHz IQ 采集带宽），参考电平 -40 dBm，DRM 解调。
- 示例使用 15 秒 IQ 文件；循环发射缓冲大小应符合发射器的可用内存。

### 生成、发射与采集

从本仓库运行，外部 DecDRM 的 `decdrm` 可执行文件须在 `PATH` 中：

```sh
decdrm tx tools/bench/drm_bench/station_iq.toml --output /tmp/drm_iq_15s.wav --duration 15
python3 tools/pluto/pluto_drm_tx.py --iq /tmp/drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

抓取前启动服务并配置 SDR 会话。在频谱中确认 400.1 MHz 处有约 10 kHz 宽的信号。抓取需要 Python `aiohttp` 和 NumPy：

```sh
python3 tools/bench/drm_capture.py --seconds 10 --out /tmp/drm_live.f32 --json /tmp/drm_live.json
```

工具订阅 `/ws?iq=1&noaudio=1`，不会配置或打开设备。它保存 IQBF wire v2 帧中的小端 float32 交织 I/Q，以及采样率、电平和传输元数据；测量前先丢弃一秒的排队帧。保存的首帧身份、中心频率、实际率与延迟必须保持一致；非法尺寸、非有限样本、重复／缺失序号、流重置和已知输入时钟倒退均拒绝。追加前检查 `--max-bytes`（默认 512 MiB），仅保存能完整容纳的帧；这是采集数据预算，不是进程总 RSS 上限。离线解码应使用上报的采样率；已提交真实夹具上报的采样率为 48828.125 Hz，而非 DRM30 内核的 48 kHz。采集电平与历史射频测量结果不能作为新会话的校准常数。

### 离线对比

已提交抓取与合成夹具见[夹具 README](../../../tests/fixtures/drm/README.md)。原生接收机夹具命令为：

```sh
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test drm_receiver live_capture -- --test-threads=1
```

此命令读取已提交夹具，不读取 `/tmp/drm_live.f32`。原生编解码器构建需要 `pkg-config` 与支持 DRM/USAC 的 FDK-AAC >= 2.0.3；参见 [AAC 夹具](#aac-夹具)。

要独立对比新采集的数据，请提供支持 `--status-socket` 的外部 Dream console 二进制，并使用采集时上报的实际采样率：

```sh
python3 tools/analysis/drm_oracle_check.py /tmp/drm_live.f32 48828.125 --dream-bin /path/to/dream
```

对照工具重采样到 48 kHz 并归一化，再写出 Dream 所需的 int16 IQ WAV。Dream 与 DecDRM 均为外部依赖；此处不提供其二进制或外部构建说明。

### 音频说明

DRM 音频必须按信令中的编解码配置解帧。text 标志置位时，每个音频逻辑帧最后四字节是文本，必须在拆分接入单元前移除，否则会错移从末尾反向读取的 SBR 负载。历史 HE-AAC 抓取每个 400 ms 帧含五个 208 字节接入单元，以 24 kHz PCM 输出 1 kHz 单音。

直接 AAC-LC 夹具使用不带 SBR 的 24 kHz 核心，输出 24 kHz。合成端到端 AAC IQ 夹具在同一核心上声明 SBR，因此报告 48 kHz 输出。应使用解码器返回的采样率与声道数，而不是由夹具文件名推断速率。流式解码还依赖跨输入块持续保存 MSC 解交织与载波校正状态。

## AAC 夹具

[夹具重序列化器](../../../tools/fixtures/drm_aac_reserialize.py) 将原始 MPEG-4 GA AAC-LC 单声道、`ONLY_LONG` 接入单元转换为 DRM 语法。它实现 VCB11 段数据、HCR 频谱重排与 CRC-8，不是通用的 DRM 音频编码器。生成器只接受 24 kHz 单声道，跳过其他窗序列；重序列化器拒绝不支持的元素、预测、脉冲、TNS 和增益控制，这条夹具生成路径不生成 SBR。FDK-AAC 提供 DRM 解码器（`TT_DRM`），但不提供 DRM 编码器。

### 帧语法

对于支持的无 TNS、无 SBR 夹具，输出接入单元为：

```text
aac_crc_bits(8)
ics_info tns_data_present ltp_data_present global_gain
section_data scale_factor_data reordered_spectral_data_length(14)
longest_codeword_length(6) spectral_data(HCR)
```

- 省略 GA 的 `id_syn_ele` 与 element tag。长窗 `ics_info` 包含 reserved(1)、window sequence(2)、window shape(1)、`max_sfb`(6)，不含 GA 的 `predictor_data_present` 位。
- `global_gain` 位于 TNS/LTP 标志之后。一般 DRM 语法可以承载 TNS 数据；夹具重序列化器未实现该负载。
- **VCB11 指虚拟码本 11，不是 11 位索引。** 此长窗路径写入 5 位码本索引。码本 11 与虚拟码本 16–31 隐含一个频带；其他码本使用 5 位段长度，转义值为 31。虚拟码本限定系数的最大绝对值。
- CRC 覆盖输出中从 `ics_info` 到 HCR 长度字段的边信息，不含频谱数据。实现使用多项式 `0x1D`、初始寄存器 `0xFF`、高位优先处理与最终按位取反；`TT_DRM` 的 CRC 字节置于负载之前。一般 CRC 边界遵循解码器的 `drmRead_CrcStartReg`/`CrcEndReg` 区域，包括适用的 TNS 数据。
- 真实 HE-AAC DRM 帧将 SBR 放在帧尾，以反向比特顺序读取，并带有独立 CRC。无 SBR 夹具省略此部分。音频解帧前必须移除文本字节；参见[台架流程](#台架流程)。

### HCR 排序

HCR 是分段状态机，不是普通的码本排序：

- 优先级为 `11, 31..16`（降序），然后是 `9/10, 7/8, 5/6, 3/4, 1/2`。
- 最大码字长度包含符号位与转义位。段宽为 `min(aMaxCwLen[cb], longest_codeword_length)`。
- 优先码字从各段起点开始写入。剩余码字按集合填入剩余空间，各集合交替写入方向。长度字段描述重排后的频谱负载长度与最长码字长度。

### 夹具生成与编解码器构建

[夹具 README](../../../tests/fixtures/drm/README.md) 维护夹具元数据、校验器构建命令与既有对比记录。直接夹具包含九个 36 字节 DRM 单元及配对的九个 320 字节 GA 单元：1 kHz 正弦波、AAC-LC、24 kHz 单声道、64 kbit/s、每帧 960 样本。解码器预热导致最初两帧静音。

生成需要外部 FDK 的 GA 输出，编码器须应用 DecDRM `crates/decdrm-fdk-sys/build.rs` 所述 960 样本补丁（`aacenc_lib.cpp`、`aacenc.cpp`、`psy_configuration.cpp`）。原始接入单元存为 `*.bin` 文件后：

```sh
python3 tools/fixtures/gen_drm_aac_fixture.py /path/to/ga_units /tmp/aac_sine_24k
```

命令输出 `.drm`、`.ga` 与 `.json` 文件，跳过非 `ONLY_LONG` 帧。元数据中的 `verification_required` 要求另行运行真实 FDK 校验器，对比 `TT_DRM` 与 `TT_MP4_RAW` 的 PCM 和音调；生成成功不表示解码验证成功。源编码器及其补丁须另行准备，此命令不提供它们。

接收机的[构建脚本](../../../dsp/core/build.rs) 要求 `pkg-config` 与支持 DRM/USAC 的原生 FDK-AAC >= 2.0.3。Wasm 构建链接已提交的 FDK 与 libxaac 静态库；独立 libxaac 诊断仅支持 wasm。原生 FDK 处理 AAC、HE-AAC 和 xHE-AAC/USAC。PCM 使用方必须采用解码器返回的采样率与声道数：直接无 SBR 夹具输出 24 kHz，合成 IQ 夹具声明 SBR，输出 48 kHz。

### 参考资料与许可

- ISO/IEC 14496-3 §4.5.4.2.4（HCR）、§4.6.2（Huffman 表）；ETSI ES 201 980 §5.3.1.2（DRM AAC 组帧）。
- 外部 FDK-AAC 源码：`libAACdec/src/aacdec_hcr.cpp`、`aacdec_hufftab.h`、`aac_rom.cpp`；`libMpegTPDec/src/tpdec_drm.cpp` 定义传输 CRC 区域。FDK-AAC 使用自身许可证。
- [DecDRM](https://github.com/CasualArclamp/DecDRM) 使用 GPL-2.0-or-later。其 `FdkDrmEncoder`、`aac/drm.rs`、`aac/hcr.rs` 是外部算法参考，不应复制源码到独立接收机中。生成夹具数据与生成器源码有不同来源；该记录由[夹具 README](../../../tests/fixtures/drm/README.md#provenance-and-licence) 维护。

本文不附带标准、外部编码器补丁或参考解码器源码。编解码回归使用版本匹配的夹具；真实 RF 验收仍需独立记录设备、信号源、采集几何与连续性证据。
