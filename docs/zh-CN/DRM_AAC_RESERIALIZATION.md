# DRM AAC 重序列化（MPEG-4 → DRM 语法）

[夹具重序列化器](../../tools/fixtures/drm_aac_reserialize.py) 将原始 MPEG-4 GA AAC-LC 单声道、`ONLY_LONG` 接入单元转换为 DRM 语法。它实现 VCB11 段数据、HCR 频谱重排与 CRC-8，不是通用的 DRM 音频编码器。生成器跳过其他窗序列，这条夹具生成路径不生成 SBR。FDK-AAC 提供 DRM 解码器（`TT_DRM`），但不提供 DRM 编码器。

## 帧语法

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
- 真实 HE-AAC DRM 帧将 SBR 放在帧尾，以反向比特顺序读取，并带有独立 CRC。无 SBR 夹具省略此部分。音频解帧前必须移除文本字节；参见[台架音频说明](DRM_BENCH.md)。

## HCR 排序

HCR 是分段状态机，不是普通的码本排序：

- 优先级为 `11, 31..16`（降序），然后是 `9/10, 7/8, 5/6, 3/4, 1/2`。
- 最大码字长度包含符号位与转义位。段宽为 `min(aMaxCwLen[cb], longest_codeword_length)`。
- 优先码字从各段起点开始写入。剩余码字按集合填入剩余空间，各集合交替写入方向。长度字段描述重排后的频谱负载长度与最长码字长度。

## 夹具生成与编解码器构建

[夹具 README](../../tests/fixtures/drm/README.md) 维护夹具元数据、校验器构建命令与既有对比记录。直接夹具包含九个 36 字节 DRM 单元及配对的九个 320 字节 GA 单元：1 kHz 正弦波、AAC-LC、24 kHz 单声道、64 kbit/s、每帧 960 样本。解码器预热导致最初两帧静音。

生成需要外部 FDK 的 GA 输出，编码器须应用 DecDRM `crates/decdrm-fdk-sys/build.rs` 所述 960 样本补丁（`aacenc_lib.cpp`、`aacenc.cpp`、`psy_configuration.cpp`）。原始接入单元存为 `*.bin` 文件后：

```sh
python3 tools/fixtures/gen_drm_aac_fixture.py /path/to/ga_units /tmp/aac_sine_24k
```

命令输出 `.drm`、`.ga` 与 `.json` 文件，跳过非 `ONLY_LONG` 帧。源编码器及其补丁须另行准备，此命令不提供它们。

接收机的[构建脚本](../../wasm/build.rs) 要求 `pkg-config` 与支持 DRM/USAC 的原生 FDK-AAC >= 2.0.3。Wasm 构建链接已提交的 FDK 与 libxaac 静态库；独立 libxaac 诊断仅支持 wasm。原生 FDK 处理 AAC、HE-AAC 和 xHE-AAC/USAC。PCM 使用方必须采用解码器返回的采样率与声道数：直接无 SBR 夹具输出 24 kHz，合成 IQ 夹具声明 SBR，输出 48 kHz。

## 参考资料与许可

- ISO/IEC 14496-3 §4.5.4.2.4（HCR）、§4.6.2（Huffman 表）；ETSI ES 201 980 §5.3.1.2（DRM AAC 组帧）。
- 外部 FDK-AAC 源码：`libAACdec/src/aacdec_hcr.cpp`、`aacdec_hufftab.h`、`aac_rom.cpp`；`libMpegTPDec/src/tpdec_drm.cpp` 定义传输 CRC 区域。FDK-AAC 使用自身许可证。
- [DecDRM](https://github.com/CasualArclamp/DecDRM) 使用 GPL-2.0-or-later。其 `FdkDrmEncoder`、`aac/drm.rs`、`aac/hcr.rs` 是外部算法参考，不应复制源码到独立接收机中。生成夹具数据与生成器源码有不同来源；该记录由[夹具 README](../../tests/fixtures/drm/README.md#provenance-and-licence) 维护。

本文不附带标准、外部编码器补丁或参考解码器源码。上述说明基于实现与已有夹具记录，本次未重新运行编解码器或硬件验证。
