# DRM AAC 重序列化（MPEG-4 → DRM 语法）——设计笔记

DRM30 使用的 AAC 语法与 MPEG-4 不同：频谱数据用 **HCR**（Huffman 码字重排）承载，
段数据用 **VCB11**（码本索引）承载，SBR 负载被移到帧尾，一个 CRC-8 覆盖特定的
边信息比特范围。fdK-AAC 有这套语法的*解码器*（`tpdec_drm.cpp` + `aacdec_hcr*.cpp`），
但**没有编码器**，所以无法直接复用 fdK-AAC 生成 DRM 音频 fixture。本笔记是独立
重实现（ISO/IEC 14496-3 §4.5.4.2.4 / ES 201 980 §5.3.1.2）的方案，即「重写」路径。

## 编码器每帧必须产出的内容

1. **段数据用 VCB11**：每段的码本以 11 位值发送；段*长度*是隐含的（由频谱数据
   推导，不同于 MPEG-4 显式发送段长度）。转义/扩展码本以不同方式标识。
2. **频谱数据用 HCR**：量化频谱系数按 MPEG-4 方式做 Huffman 编码，但码字被
   *重排*——按码本分组——其前有两个边信息字段：`reordered_spectral_data_length`
   与 `longest_codeword_length`，随后是让解码器定位每个码字的分段网格。
   （即 `aacdec_hcr.cpp::HcrDecoder` 的逆过程。）
3. **SBR 负载移到帧尾**（其自身的 8 位 CRC 由 SBR 编码器给出；对无 SBR 的 AAC-LC
   信号此步骤省略）。
4. **`aac_crc_bits`**：对 DRM 解码器校验的边信息比特范围（`tpdec_drm.cpp` 的
   `drmRead_CrcStartReg`/`CrcEndReg`）做 CRC-8（poly 0x1D，init 0xFFFF），在
   `TT_DRM` 下置于帧前传输。

## 实现方案（独立、基于规范）

- **第 1 步——解析 MPEG-4 AAC**：按位读取原始访问单元（来自 fdK-AAC 的 MPEG-4
  编码器或 ffmpeg）：ICS 信息、段数据（每段码本）、比例因子、频谱 Huffman 码字。
  Huffman 码本是标准 AAC 表（ISO 14496-3 §4.6.2）；参考 fdK-AAC 的 `aacdec_hufftab.h`
  / `aac_rom.cpp`。
- **第 2 步——VCB11**：把段码本列表重编码为 11 位条目。
- **第 3 步——HCR**：按码本排序码字，计算 `longest_codeword_length` 与分段网格，
  输出重排后的频谱比特（`HcrInit`/`HcrDecoder` 排序的逆过程）。
- **第 4 步——SBR + CRC**：把 SBR 负载追加到帧尾，并对 `tpdec_drm.cpp` 覆盖的精确
  范围计算边信息 CRC-8。

## 参考资料

- ISO/IEC 14496-3 §4.5.4.2.4（HCR）、§4.6.2（Huffman 表）。
- ES 201 980 §5.3.1.2（DRM AAC 帧 + `aac_crc_bits`）。
- fdK-AAC：`libAACdec/src/aacdec_hcr.cpp`（HCR 解码）、`libMpegTPDec/src/tpdec_drm.cpp`
  （CRC 范围）、`libAACdec/src/aac_rom.cpp`（Huffman 表）。
- DecDRM `FdkDrmEncoder`（GPL，仅作算法参考——不可复制）。

## 精确的 DRM AAC SCE 帧布局（DecDRM `aac/drm.rs`、`el_drm_sce`）

```text
[id_syn_ele element_tag]  [ics_info tns_data_present ltp_data_present global_gain
 section_data scale_factor_data hcr_lengths tns_data]  spectral_data(HCR)
```

- `ics_info` = ics_reserved(1) window_sequence(2) window_shape(1) max_sfb(6 或 4+7)
  ——**没有** GA 的 `predictor_data_present` 位。
- `global_gain` 移到 tns/ltp 标志**之后**（DRM 的字段序与 GA 不同）。
- 方括号内的部分（ics_info … tns_data）由 `aac_crc_bits`（CRC-8）覆盖。
- `aac_crc_bits` 字节在 `TT_DRM` 下前置到帧首。

## HCR 重排是状态机，不是普通排序（DecDRM `aac/hcr.rs`）

- 码字按**码本优先级**排序，而非码本编号：`11, 31..16（降序）, 9/10, 7/8, 5/6,
  3/4, 1/2`（`aCbPriority`）。
- `aMaxCwLen[cb]` 是**含符号位与转义位**的最大码字长度；分段宽度为
  `min(aMaxCwLen[cb], length_of_longest_codeword)`。
- 最高优先级的码字（PCW）起始各分段；其余码字按*集合*分布在剩余空间中，每个集合
  交替读取方向。编码器即解码器控制流程中把每个「读一位」替换为「在此写入该码字
  的下一位」。

一个更简单的里程碑是：先对单码本的 AAC-LC 帧（无 SBR）实现第 1+2+4 步，待 VCB11
组帧能通过 fdK-AAC `TT_DRM` 解码器往返后再加入 HCR（第 3 步）。
