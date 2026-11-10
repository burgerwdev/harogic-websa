# 官方 FT8 源码核对，2026-11-10

源码固定提交，与同输入对照所用二进制版本分别记录：

- WSJT-X 官方 `WSJTX/wsjtx`，提交
  `967c85a6119110788b0df3782cf796485b2cee14`：
  [ft8b.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/ft8b.f90)、
  [baseline.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/baseline.f90)、
  [get_spectrum_baseline.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/get_spectrum_baseline.f90)、
  [sync8.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/sync8.f90)、
  [ft8_decode.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8_decode.f90)。
- JTDX 官方 `jtdx-project/jtdx`，提交
  `2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb`：
  [ft8b.f90](https://github.com/jtdx-project/jtdx/blob/2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb/lib/ft8b.f90)、
  [ft8_downsample.f90](https://github.com/jtdx-project/jtdx/blob/2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb/lib/ft8_downsample.f90)。
- 同 IQ 对照继续使用 Debian WSJT-X **2.7.0+repack-1** 的 `jt9`、`ft8sim`，
  SHA-256 见验证 JSON。不能将此发行包视为由上述源码提交构建。

| 环节 | WSJT-X | JTDX |
|---|---|---|
| 噪声统计 | `ft8b` 累计 79 个已解码音调的功率；备用参考取 `mod(itone+4,7)` 音调。通常路径使用平均谱的 `xbase`：分十段取最低 10% 点拟合四次多项式。`sync8` 另用 40% 分位数归一化候选同步对比度，不能与报告 SNR 的背景估计混为一谈。 | 每码元比较已解码音调功率与其他七音调的平均功率，再平均这些比值并减一；内部噪声分母下限为 0.01。 |
| 经验及尺度修正 | 基线增加 0.65 dB，`xbase` 转换含 −40 dB，第二种比值含 `3e6`，最终均减 27 dB；初始化哨兵及 `arg > 0.1` 也属于特定尺度下的启发式处理。 | 减 26.5 dB；高于 +7 dB 后非线性上调，+30/+40 dB 上方继续修正；低于 −17 dB 有非线性弱信号修正。这些是经验报告校正。 |
| 报告限幅 | 在误解码检查之后，将小于 −25 dB 的报告提升至 −25 dB。 | 上限 +49 dB；非 AP 下限 −23 dB，AP 下限 −24 dB；FT8S 分支还有 −26 dB 下限。 |
| 误解码筛选 | 同步计数、LDPC/CRC、载荷检查；当 `nsync <= 10` 且限幅前 SNR 小于 −25 dB 时拒绝可能的误解码，随后才有报告下限。 | LDPC/CRC、硬判决错误/距离形成的 `qual`、时延、AP、消息及呼号检查和依赖 SNR 的分支。这些影响消息接收，与显示限幅不同。 |

适合原始复数 IQ 的方法：使用 CRC 解出的音调，包含 Costas 和数据；精校时频，
以局部噪声扣除 ON 匹配滤波功率，再用**实际 ENBW / 2500 Hz**换算。即使消息
通过抵消后才恢复，测量也使用原始接收样本。复数双边噪声与实数音频单边噪声的
注入功率要分别正确换算。

不照搬音频归一化常数、低分位偏置、分母固定下限、非线性报告修正或限幅。
尤其不能未经统计校正地挑选最低噪声点，否则会抬高 SNR。本实现用指数次序统计
的精确期望校正中位数，并用截断指数分布的条件均值校正上尾剔除；四倍尺度阈值和
十二次迭代上限控制稳健性与开销，不控制可以显示的 SNR 范围。偏差另用独立种子验证。

测量器不替换或增加消息拒绝条件。原有 Costas 精校、相干联合判决、BP/OSD、
LDPC/CRC 继续工作；测量失败只影响消息的测量附注。质量标记不是 WSJT-X/JTDX
的解码置信度，也不能据此丢弃已解码消息。

多数参考持续受污染时无法保证恢复热噪声；精确同频功率不可分离。背景急剧弯曲、
残余频偏、GFSK 失配和衰落均可能造成偏差。质量提示不是精确测量或误差保证。
详见 [SNR](SNR.md)。
