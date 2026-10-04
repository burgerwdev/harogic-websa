# DRM 解调器——交接：状态、证据、下一步

本页面向接手的人，包括换模型之后的下一轮。实测数据在 `docs/zh-CN/DRM_BENCH.md`，本页记录
状态与计划。

## 目前进展

已可用并有测试或台面证据：

- 台面真实抓取能够锁定。接收机从检测到的带边求出频偏的整载波部分，从保护间隔相关求出小数
  部分；并逐个尝试超帧相位，取 SDC CRC 通过的那一个。
- 读数显示台名、鲁棒模式、带宽、码率、编码和 FAC SNR，MSC 通过校验。读数还会每五秒重新
  上报一次，因此清空窗口后能自行恢复。
- 用过 DRM 之后再切其他解调模式不需要刷新页面：trap 掉的 worker 会被丢弃，下一次切换模式
  会新建一个。
- **音频已能解码。** 真实 HE-AAC 码流（12 kHz 核心 + SBR）在 wasm 内解码出非静音的 24 kHz
  PCM：worker 的分块喂入和整段推送都可以。FDK trap 已在根因上修复（对齐分配器必须清零，
  同 FDK 的 `genericStds`）；SBR 标志不再被剥离；后锁定通路能产出音频（MSC 去交织器跨
  pass 持久化，载频频移校正累加）。细节与证据链见 `docs/zh-CN/DRM_BENCH.md` 缺陷 3。
- 测试：`cargo test --test drm_fixture` 4/4，真实抓取测试 5/5（含流式音频回归），物理层
  鲁棒性 5/5，DRM wasm 测试 7/7（`drmLiveAudio`、`drmAudioEndToEnd`、`drmAudio`、
  `drmXaac`、`workerRecovery`）。

未完成：

- 2026-10-04 台面已验证：实时抓取 native 解出 35 个接入单元，wasm 分块路径产出 57 600 个
  非静音 24 kHz 样本；`tools/e2e/drm_switch.py` 通过——DRM 在浏览器内锁定，往返 AM 后再次
  锁定，全程无刷新。台面有个怪癖要记住：DDC 的数字增益因服务实例而异，只有热电平能锁定——
  见 `docs/zh-CN/DRM_BENCH.md` 的台面实测一节。
- xHE-AAC 端到端（coding 3 → libxaac）从未用真实 xHE 码流验证过；DecDRM 可以发一路
  （`codec = "xhe-aac"`）。HE-AAC v2 已接线但未验证。
- 信道估计仍是简化的逐符号线性插值。物理层套件在台面抓取上已全部通过，所以下面的 Wiener
  移植只在台面实测需要时才值得做。

## 花了时间才确认的台面事实

- **参考电平 −35 到 −45 dBm**：此时带内对比度 25 到 27 dB。−40 dBm 时只有 6 dB，无法锁定。
  **每次台面运行前都要重新设置**：preset 与后端自身的参考电平调整都会覆盖它；本目标的两次
  会话都遇到过设置被恢复后信号消失的情况。
- 用 `tools/pluto_drm_tx.py` 配合 15 秒文件发射。60 秒文件会产生 250 MB 循环缓冲，Pluto 会以
  `EFAULT` 拒绝推送。
- 台面码流是标准 DRM HE-AAC 配置：12 kHz 核心 + SBR，每个接入单元 208 字节，每 400 ms 帧五
  个。与 1048 字节的流帧对得上，说明解帧与码流一致。
- 合成夹具在 SDC 里声称 SBR，但负载中没有 SBR 数据（即 “no-SBR smoke” 情况），所以它走的
  是与台面码流不同的路径。

## 音频缺陷，按发现顺序

1. FDK 的 wasm 解码器在台面接入单元上 trap。是否 trap 取决于堆布局：同一份产物有时通过、有时
   失败。
2. `wasm/fdk/shim.cpp` 忽略了 `FDKaalloc(size, alignment)` 要求的对齐，因此 FDK 的 SBR 对齐
   缓冲区只保证到 16 字节。修好它消除了第一个故障，但暴露出解码内部的第二个故障，而且同一处
   修复让夹具重新 trap。该修复尚未入库，需要单独调查。
3. 接入单元需要背后有余量的缓冲区。FDK 会在核心帧之后寻找 SBR 负载，而一个声称 SBR 却不携带
   SBR 的码流会让它读过接入单元之外。已入库：`AacDecoder::decode` 现在把接入单元拷进一个背后
   带 512 个零字节的缓冲区，音频冒烟测试连续三次通过。
4. 音频需要在锁定之后再解一次，因为长交织器要跨五帧才填满（实测：3 秒窗口能产出 2 到 3 个
   MSC 帧和 5 个接入单元）。
5. 这一次必须带上绝对行号，否则超帧相位是错的。

## 值得参考的实现

下面两个项目是可用的 DRM 接收机。它们的代码不能直接搬进本分支，但在改我们自己的实现之前，
它们的 DRM 与音频处理值得一读。

- 同仓库的另一个 worktree `/home/hui/git/harogic-websa-drm-dream`：围绕 Dream 可执行文件、已
  验证过的 Python 路径。建议读 `web_sa/drm_dream/decoder.py`、`tools/drm_dream/probe_fixture.py`、
  `tools/drm_dream/loopback_probe.py` 和 `docs/en/DRM_DREAM.md`。里面记录了 DDC 馈送需要注意
  什么（实测速率、电平）以及 Dream 报告了什么。
- DecDRM 检出 `/home/hui/git/DecDRM`：生成台面信号的发射机，旁边就是它的接收机。
  `crates/decdrm-codecs/src/aac/drm.rs` 是 DRM AAC 接入单元（含 SBR 负载与 CRC 字节）最清楚的
  参考。
- FDK 源码 `/home/hui/git/fdk-aac`：`libMpegTPDec/src/tpdec_drm.cpp` 说明 DRM 传输期待什么，
  `libAACdec/include/aacdecoder_lib.h` 列出可用的参数与流信息结构。

DecDRM 的接收机模块与我们接收链的薄弱环节一一对应，而且同为 Rust：

| DecDRM 模块 | 作用 | 我们的现状 |
| --- | --- | --- |
| `rx/freqacq.rs` | 用三个连续频率导频（750/2250/3000 Hz）在 6 x 1024 点 FFT 里做粗载波捕获，同时搜索镜像图案 | 保护间隔相位只能看到小数部分，带边只能看到整载波 |
| `rx/timesync.rs` | 先低通到 ±4.5 kHz 再四倍抽取的保护间隔相关，与 Dream 的路径一致 | 全速率相关，只做一次静态对齐 |
| `rx/chanest/`（`time_wiener.rs`、`track.rs`） | 时间维 Wiener 信道估计加跟踪 | 每符号在离散导频间做线性插值 |
| `rx/framesync.rs`、`rx/ofdm.rs`、`rx/mscdec.rs` | 帧同步、OFDM 解调与 MSC 解码的流式阶段 | 对有界缓冲的一次性解码 |

读它们是为了算法；DecDRM 是 GPL-2.0-or-later，因此不复制代码。

目标明确不把 Dream 子进程引入本分支。读这些项目是为了算法，而不是再加一个解码器。

**下一轮计划**：移植 DecDRM/Dream 的 `freqacq.rs`（基于导频的粗载波捕获）、
`rx/chanest/`（时间维 Wiener 信道估计加跟踪）、`rx/timesync.rs`（低通+抽取的保护间隔
相关）。这三个解决剩余故障：载波锚点、信道估计质量、真实信号的符号定时。然后去掉
SBR 保护并测试音频解码（上一轮的 AU 填充应已防止 FDK trap）。

**台面参考电平**：每次运行前设为 **−40 dBm**。preset 与后端自身的参考电平调整会覆盖
它。用户确认此设置下信号可见。

## 音频编码覆盖

接收机按 SDC 音频编码字段分发：0 是 AAC（描述符里还有 SBR 与声道模式标志），3 是 xHE-AAC。
两条解码路径都已接线并提交。

| 编码 | SDC | DecDRM 台站配置 | 接收机路径 | 状态 |
| --- | --- | --- | --- | --- |
| AAC-LC | coding 0，SBR 关 | `codec = "aac"` | FDK TT_DRM | 可解码（音频夹具） |
| HE-AAC | coding 0，SBR 开，单声道 | `codec = "he-aac"` | FDK TT_DRM | 24 kHz 夹具可解码；真实台面码流被上面的 FDK 故障阻塞 |
| HE-AAC v2 | coding 0，SBR 开，立体声 | `codec = "he-aac-v2"` + `stereo` | FDK TT_DRM | 已接线，台面未测 |
| xHE-AAC | coding 3 | `codec = "xhe-aac"` | libxaac，用 SDC 里的 AudioSpecificConfig | 已接线；USAC 冒烟测试把 45 个接入单元解成非静音 PCM；DRM 端到端路径尚未测 |

给下一轮的两点提示：

- 音频夹具的 SDC 声称 SBR，但负载里没有 SBR 数据，因此 FDK 被配置成等待一个并不存在的
  尾段。合成夹具因此不是符合标准的 SBR 码流；台面码流是。
- xHE-AAC 的端到端路径（coding 3 经 DRM 接收机进 libxaac）从未用真实 xHE 码流驱动过。
  DecDRM 可以发射：把台站配置的 `codec` 设为 `xhe-aac`。

## 后锁定通路的结论

早先把锅扣给逐符号信道估计是错的。pass 的 SNR 逐帧崩塌（20 dB 跌到 1.7 dB）有两个具体的
原因，都已修复：

- MSC 的 `CellDeinterleaver` 每次 pass 重建，长交织器的五帧填充耗光了整个 3 秒窗口。现在它
  跨 pass 持久化，帧带绝对序号，重叠窗口不会重复喂数。
- `remove_carrier_offset` 覆盖而不是累加 `mix_w`。整载波锚点校正之后，锁定后到达的每个
  样本都少转了第一次（分数部分，约 120 Hz）校正。锁定通路永远看不到它——它把整个缓冲区
  一次性原位旋转——这正是整段推送一直能掩盖它的原因。

两处修好后，一次流式 pass 就能从 6 秒抓取里产出 25 个接入单元，音频正常播放。在这个信号
上，信道估计本身从来不是问题。

## 修复用的参考模块

DecDRM 的接收机模块正好解决这些弱点：

- `rx/freqacq.rs`：用三个连续频率导频（750、2250、3000 Hz）在 6 x 1024 点 FFT 里做粗
  载波捕获，同时搜索镜像图案。
- `rx/timesync.rs`：先低通到 ±4.5 kHz 再四倍抽取的保护间隔相关，与 Dream 的路径一致。
- `rx/chanest/`（`time_wiener.rs`、`track.rs`）：时间维 Wiener 信道估计加跟踪，替代我们的
  逐符号线性插值。
- `rx/framesync.rs`、`rx/ofdm.rs`、`rx/mscdec.rs`：帧同步、OFDM 解调与 MSC 解码的流式阶段。

它们与我们的 wasm 同为 Rust，且同属一个项目家族。DecDRM 是 GPL-2.0-or-later，读其算法
后独立实现。

## 下一步（按顺序）

1. 台面在线验证音频：运行前把 ref level 设到 -35..-45 dBm，用 `tools/pluto_drm_tx.py` 发射，
   `tools/drm_capture.py` 抓取后经 wasm 路径解码（`frontend/scripts/drm_live_audio.mjs`）
   应显示非静音 PCM，并且 preset 切换（进 DRM 再切回）不刷新页面。
2. 在台面上端到端驱动 xHE-AAC：在 DecDRM 的台站配置里设 `codec = "xhe-aac"`，抓取后检查
   libxaac 路径（`coding 3`）与真实 xHE 码流。
3. 同样方式验证 HE-AAC v2（SBR + PS，立体声）。
4. 如果台面实测暴露信道估计或定时问题，再移植下面的 DecDRM 模块；今天的离线套件已全部
   通过，这一步由证据驱动，不是自动必做。

## 剩余的差距：信道估计（下一轮的工作）

音频链路已修好并验证（见 `docs/zh-CN/DRM_BENCH.md`）。剩下的是解调器的信道估计，而参考接收机
在同一捕获上量出了差距有多大：

| 接收机 | FAC | MSC 帧 | 音频 |
| --- | --- | --- | --- |
| DecDRM 的 `decdrm rx` 解 `live30.f32`（30 秒，MER 17.8 dB） | 好 64 / 坏 9 | 71（好 40） | 200 帧（115 掩盖）|
| 我们，同一文件 | 好 40 / 坏 52 | 64 | 55 个接入单元（11 个 super frame）|

同样信号下参考解出的音频约为我们的四倍。原因在 `wasm/src/digital/drm/chanest.rs`：跨散射导频的
逐符号线性插值，时间维不插值、也不做自适应。参考实现（移植自 Dream）的做法是：

1. 逐符号取出增益参考（散射）导频网格上的信道；
2. **时间维 Wiener 插值**（依据多普勒/时延统计）——弱信号与衰落信道靠的就是这一步
   （`rx/chanest/time_wiener.rs`，对应 Dream 的 `CChannelEstimation::UpdateTimeWiener`）；
3. **频率维 Wiener 插值**，从导频网格插到每个载波
   （`rx/chanest/mod.rs::update_freq_wiener`，Levinson-Durbin 求解）；
4. **冲激响应跟踪**（`rx/chanest/track.rs`，Dream 的 `CTrack`）：从功率时延谱测时延扩展、多普勒
   扩展与采样率偏差，喂给 (2)(3) 的统计量，并给定时环路提供校正量。

移植源（同为 Rust）：`/home/hui/git/DecDRM/crates/decdrm-core/src/rx/chanest/{mod.rs,
time_wiener.rs, track.rs}` 与 `rx/scatter.rs`（导频/DSP 辅助），以及
`crates/decdrm-core/src/dsp/` 里的 `levinson`、`iir1`、`sinc`。Dream 原版在 `src/chanest/`。

在本仓库里的改动形态：`chanest::equalize_symbol(map, sym, cells) -> EqSymbol` 是无状态、逐符号的；
Wiener 估计器是有状态的（时间滤波跨多个符号，因此符号进入后要过几个符号才输出），还需要 SNR、
时延扩展与多普勒扩展。因此它变成由 `DrmReceiver` 持有的 `ChannelEstimator`，逐符号喂入，输出延迟
为 `time_wiener::delay()` 个符号；`decode()` 需要缓存输入符号、消费输出的均衡符号。它给出的
SNR/MER 应当取代我们现在的 `snr_db` 读数（现在来自 FAC 判决，读数偏低：参考报 MER 17.8 dB 时
我们报 -11 dB）。

分步推进，每步都可单独验证：

1. 导频网格 + 时间维 Wiener（取代只在频率维插值的做法）——先看到 FAC 错误数下降，再看到 MSC 帧数上升；
2. 频率维 Wiener 与 SNR 自适应；
3. 冲激响应跟踪（时延/多普勒/SRO），它同时给定时环路提供校正，取代我们固定的网格；
4. 读数：像参考实现那样报告估计器的 SNR 与 MER。

验收：在 `live30.f32` 上我们解出的音频帧数接近参考（以 `decdrm rx` 为准），并且 native 套件
（`drm_fixture`、`drm_live_fixture`、`drm_phy_robustness`）保持全绿。测量用的捕获在 `/tmp`
（`live30.f32`、`live60.f32`、`lvl.f32`）；可用 `tools/drm_capture.py` 重新抓取。

## 移植进度（分支 `feature/drm-dream-port`）

旧接收机已放弃；本分支按 Dream 的阶段顺序用 Rust 重新实现，保持 MIT 干净（Dream 与 DecDRM 只作
参考阅读，不复制代码）。每个阶段落地时都带测试，并用参考实现自己的测量值交叉核对：

| 阶段 | 状态 | 证据 |
| --- | --- | --- |
| `drm2::params` —— 鲁棒模式几何（A–D） | 完成 | 逐项对照 Dream 的 `tables/TableDRMGlobal.h`；400 ms 帧不变量有测试固定 |
| `drm2::dsp` —— 复数类型、DFT（radix-2 / Bluestein） | 完成 | 单频复指数在 1024/1152/704/448/6144 各长度下只落在一个 bin |
| `drm2::sync::freqacq` —— 三导频粗载波捕获 | 完成 | 已提交台面夹具捕获到 **+121 Hz**（与 `DRM_BENCH.md` 记录值相差一个载波内）；合成夹具可捕获；反相信号报告为反相；噪声不误捕 |
| `drm2::sync::timesync` —— 保护间隔相关（低通+抽取）、模式检测、定时与跟踪 | 下一步 | |
| `drm2::sync::framesync` —— 由时间导频定帧相位 | 随 task 3 | 时间导频位于解调后的信元里，因此需要先有 OFDM 解调与信元映射（task 3）才能编写与测试；并入该任务，而不是在此凭空猜 |
| `drm2::cellmap` + `drm2::tables` —— DRM 信元布局（模式 A–D × 各占用） | 完成 | 移植自本仓库自己的旧实现（MIT、按规范推导、台面验证过）；等价性测试逐格检查所有合法模式/占用组合（含分类与导频复数值），并有规范锚点（65 个 FAC 信元、模式 B/10 kHz 每帧 2337 个 MSC 信元、每符号 3 个连续导频） |
| `drm2::ofdm` —— FFT 解调出映射表信元 | 完成 | 在夹具真实窗口上与旧链解调器逐样本一致；散射导频比值一致（1387 个导频，分散度 0.27） |
| `drm2::sync::framesync` —— 由时间导频定帧相位 | 完成 | 合成夹具的相位在搜索窗整体平移若干帧后不变，台面夹具同样能同步 |
| `drm2::sync::nco` —— 捕获与解调之间的载波偏移移除 | 完成 | 能把测得的偏移上的单音精确搬到直流，且相邻块之间无相位跳变 |
| `drm2::dsp::levinson` —— Dream 的 Wiener 滤波所用的 Haykin 递归 | 完成 | 对角与 2x2 系统均正确（第一版写错，被对角系统测试抓住） |
| `drm2::sync::finefreq` —— 由连续导频估计残余载波偏移 | 完成 | 向夹具注入 -2/-0.5/+0.5/+2 Hz，估计值误差 <0.1 Hz。未决：实时捕获上粗校后测得 +2.62 Hz，而残余搜索显示 -1.0 Hz 更优（3.7 dB）——两者不一致：可能是连续导频的相位旋转除了频偏还含定时/采样率漂移，或搜索的最优点并非频偏的最优点 |
| `drm2::chanest` —— 导频格点、时间插值、频率维线性插值、FAC 判决 MER | 进行中 | **干净夹具全链路 FAC MER 达 42 dB**（捕获、NCO、定时、解调、估计、均衡），而旧链的逐符号均衡器把同样信元落在距 4-QAM 点约 9° 以内。此前的负 MER 出在 Wiener 的 tap 相位而非结构：线性路径把星座精确落点，因此移除 Wiener（其 Levinson 求解器保留在 `dsp/` 并带测试），作为后续精化，以"与旧均衡器对照"为验收。未决：实时捕获最好只有 **+3.7 dB**，参考为 17.8 dB。对残余载波偏移做的一维扫描（定时固定，±6 Hz、0.5 Hz 步进，`sweep_residual_offset_with_timing_fixed`）最高也只到 3.7 dB 且曲线抖动——**没有任何纯频偏能把它救到可用**，所以缺口不在频偏。另两个扫描排除了其余嫌疑：按 -200~+200 ppm 的假定源速率误差重采样，MER 仍在 -4.8~-1.1 dB（`sweep_sample_rate_error_on_the_live_capture`）；帧相位也没有一个突出（`sweep_frame_phase_on_the_live_capture`，-7~+2.8 dB）。剩下的、也是数字指向的，是**频率维信道估计**：参考在这段捕获上测得约 1 ms 的时延扩展，其相干带宽（约 160 Hz）窄于散射导频的 281 Hz 格点间距（6 个载波），所以格点间线性插值跟不住信道。这正是 Dream 的频率维 Wiener 的用途；估计器的线性兜底只在干净平坦的夹具上达到 42 dB。Wiener 实现在提交历史里，但它的 tap 相位约定是在**格点索引修复之前**（当时信道多为零）测的，因此现在必须重新应用并重新测量：相位距离应以**格点步长**而非载波计，因为我们的格点不从载波 0 开始。下一步：Dream 的 `TimeWiener`（按多普勒自适应的时域 Wiener）与冲激响应跟踪（时延/多普勒/SRO），这是本任务另一半。**这里记录一次时间维 Wiener 的尝试，以免原样重做**：因果滤波（Dream 的高斯多普勒相关 `R(tau)=exp(-2pi^2 sigma^2 Ts^2 tau^2)`、滤波长度 A5/B7/C9/D9、sigma 0.05 Hz、Levinson tap、最新样本优先次序）在两个捕获上都**更差**（干净 42.4 → 25.0 dB，实时 -1.4 → -8.9 dB），已回退。下一次应更贴近参考的 `time_wiener.rs`：按相位设计并**带上频偏相位项**、**对称窗 + 输出延迟**、以及用时间相关估计多普勒（其 60 秒时间常数）而不是固定 sigma。 |
| `drm2::fac`、`sdc`、`mlc`、`msc`、`audio` | 待做 | 见任务清单 |

两条要记住的事实：

- **Dream 没有模式 E 几何。** `tables/TableDRMGlobal.h` 定义 `NUM_ROBUSTNESS_MODES 4`（A–D），
  它对模式 E 的唯一提及是音频超帧的帧数。因此模式 E / DRM+（VHF）无法从 Dream 移植，需要 DRM+
  规范或别的参考；它是独立的一项任务。
- **测试读夹具的路径**：Rust 测试的工作目录是 `wasm/`，已提交夹具在 `../tests/fixtures/...`。
  `/tmp` 里的捕获只是"有更好"：测试要以文件存在为前提，且**不要**断言它的绝对载波偏移（每次
  会话都不同）。

## 环境注意

- 负载高时后端会卡住（日志里的 “SDR stream stalled (watchdog)”、65 ms 的采集步进）。台面测试前
  用 `./stop.sh && ./run.sh` 重启服务。
- 本机上无头 Chromium 会崩溃（`chrome-headless-shell` 在合成器里 trap），UI 探针请用 Playwright
  的 Firefox。`tools/e2e/demod_switch.py` 仍在用 Chromium。
- 启动 Pluto 发射机请用子 shell（`( nohup ... & )`）；把 `&` 与 `&&` 串在同一行里已经多次静默
  失败。
