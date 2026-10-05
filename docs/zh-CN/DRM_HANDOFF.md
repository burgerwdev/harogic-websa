# DRM 解调器——交接：状态、证据、下一步

本页面向接手的人，包括换模型之后的下一轮。实测数据在 `docs/zh-CN/DRM_BENCH.md`，本页记录
状态与计划。

## 目前进展
接收机已换成 `wasm/src/digital/drm/` 里按 Dream 阶段结构移植的链路；旧接收机已删除，
`digital::drm` 现在是唯一的 DRM 解调器。

已可用并有测试或台面证据：

- **接收机能端到端解出提交的台面抓取。** 按 worker 的 3248 样本分块喂入即可锁定，读出
  `SAN90 DRM BENCH`，FAC 13 块 / 0 CRC 错误，8 个 MSC 复用帧，解帧出 40 个 HE-AAC 接入
  单元；wasm 路径产出 76 800 个非静音 24 kHz 样本，带台面 1 kHz 音（`drmLiveAudio` 门禁）。
  同一条链路在 native 下由 `wasm/tests/drm_receiver.rs` 验证。
- **流式正确。** 分块解码与整段一次性解码在提交抓取上完全一致——帧相位、FAC 块、台名、
  MSC 帧和音频接入单元都相同。此前的 wasm 与 native 差异（wasm 下 0 个 FAC 块）是流式路径
  过早提交帧相位、并用只适用于批处理的帧索引方式拼装 MSC 超帧；两者都已修复。
- **信道估计用的是精确的线性时间插值**，即参考实现在其定时/SRO 环闭合之前的默认路径。
  多普勒自适应的时域 Wiener 已移植且可切换，但默认不启用：在干净台面抓取上它（正确的）
  信道估计仍会让频域 Wiener 过度平滑掉残余定时斜坡，从而破坏 64-QAM 的 MSC。
- **读数**显示台名、模式、带宽、FAC/MSC/音频计数和估计 SNR；重新调谐会复位数字解调器，
  上一个频道的台名与元数据不会残留。
- **测试全绿：** `cargo test`（lib + `tests/drm_receiver.rs`）、前端 DRM wasm 测试
  （`drmLiveAudio`、`drmAudio`、`drmXaac`、`drmAudioEndToEnd`）以及 `make ci`。

未完成：

- **本轮台面环路。** Pluto 在 400.1 MHz 发射 `drm_iq_15s.wav`，SAN-90 把浏览器用的基带送出。
  ref -42 / -46 / -48 dBm 下新抓的 6 秒抓取都能通过 wasm 接收机端到端解出：`SAN90 DRM BENCH`，
  FAC 13–14 块 / 0 错误，8 个 MSC 帧，40 个音频接入单元，以及 76 800 个非静音 24 kHz PCM
  样本，其频谱峰值正好是台面 1 kHz 音（900–1100 Hz 占 99.7% 能量）。电平窗口很窄：ref -40 与
  ref -50 dBm 都解不出来（即交接里记的"只有热电平能锁定"怪癖），所以可复现的验收仍是同一台面
  抓取、已提交的那份。
- 浏览器 e2e（`tools/e2e/drm_switch.py`）本轮被基带投递卡住：DSP worker 每秒只收到约一个
  3248 样本块（后端日志 `acquisition step took 65 ms`），所以 DRM 读数一直填不上；同一个 wasm
  接收机离线 0.5 秒就能解出同样的抓取。音频验收因此走 `drmLiveAudio` wasm 门禁
  （76 800 个非静音样本）。
- **真实短波。** 抓了四个 HF 频点（9755、11620、5875、3955 kHz，各 6 秒，ref -40 dBm），
  都没有 DRM 信号（未锁定）。能否收到取决于传播和是否有电台在播。
- xHE-AAC（coding 3）已在已提交的台面夹具 `drm_live_xhe_modeB_so3_48828.f32` 上端到端解码。
  有状态超帧解帧器恢复出 USAC 接入单元（50 个，参考 51 个），再交由 FDK 的 `TT_DRM` 解码器——
  这正是参考接收机自己的路径（`open_decoder(DrmAudioCoding::XheAac)`）；libxaac 只是编码器，其
  mp4 模式解码器会拒绝紧凑的 DRM Static Config，所以那条路已弃用。`drmCodecs` wasm 测试通过
  出厂 ABI 驱动接收机，产出 102 400 个非静音 24 kHz PCM 样本，带 1 kHz 音（900–1100 Hz 占
  99.6% 能量）。
- HE-AAC v2（AAC + SBR + 参数立体声）已在台面验证：Pluto 发射 DecDRM 生成的 `heaacv2` 码流
  （12 kHz 核心），实拍抓取解出 153 600 个 24 kHz 立体声 PCM 样本，左右声道都是 1 kHz 音
  （900–1100 Hz 占 99.7% 能量，L/R 相关 1.0），40 个音频接入单元。
- **DRM30 四个模式全部端到端解出。** 真实 DecDRM 发射的已提交夹具（HE-AAC 单声道、12 kHz
  核心、模式 A/C/D、SO3）与模式 B 并列；MSC 拼装已改为按模式通用（此前写死模式 B 的每帧 15
  符号和 45 个桶，模式 C/D 因此解不出），每个模式都能锁定、FAC 无 CRC 错误、解帧出 55 个音频
  接入单元。
- **模式 E / DRM+ 已接入同一条接收链。** 当前分支单独提供 `drmplus` 插件：96 kHz/100 kHz 信道化输入、模式 E FAC（116 位、1/4 码率、两组业务参数、四帧 identity/toggle）、模式 E SDC/MSC 码率与深度 6 交织、AFS 感知信元映射、成对的 200 ms 音频逻辑帧，以及 AAC/xHE 音频接入。确定性的 `drm_modeE_so0_96k_aac.f32` 已通过 native FAC/SDC/MSC/AU 检查和 48/44.1 kHz 输出设置的 wasm PCM 门禁；注入 ±100 Hz 频偏也能通过。这里是合成信号证据，环境中没有真实 VHF DRM+ 录音或发射机，因此真实传播与重配置组合仍待验证。
- 定时/SRO 环现已闭合：一旦启用跟踪，脉冲响应跟踪器的 `timing_adjust` 与 `sro_delta_hz` 会反馈
  回 `TimeSync` 的窗口（`adjust_timing`/`adjust_sro`），FFT 窗口保持对齐，信道不再带残余定时
  斜坡。多普勒自适应的时域 Wiener 已移植且可切换，但默认仍关闭；环闭合后应在台面抓取上重新
  测量，若改善 MSC 解码则启用。

## 2026-10-05：缺口是缺少时域载波跟踪

`/tmp/live30.f32` 本轮独立验证过：转成立体声 WAV（I 左 / Q 右，48 kHz）后用
`decdrm rx --format iq --no-auto-flip` 解码得 **FAC ok 64 bad 9 · MSC 71（ok 40）· 200 音频帧
（115 掩盖）· MER 17.8 dB**，台名 `SAN90 DRM BENCH`，时延 0.8 ms，多普勒约 1.4 Hz，SRO
−0.14 Hz。样本可信——超驱的 RMS（64，对干净夹具的 0.25）是 `DRM_BENCH.md` 已记录的 DDC
数字增益怪癖，不是缺陷：参考接收机就在这个电平上锁定。归一化幅度没有用，因为打败我们的是
信道/传播，不是电平。

根因（直接在解调后的导频上测得）：

- 一次性的粗+细校正之后，剩余载波偏移**随秒级漂移**（约 −8 到 +13 Hz，均值 +2.4 Hz）。
  一次性的细校正只去掉均值；漂移的剩余偏移让信道持续旋转，3 符号的时间插值跟不上，其载波
  间干扰把冲激响应涂抹开。参考接收机从频率导频连续跟踪偏移并在时域施加（`framesync.rs` 的
  `freq_delta_hz` 加每符号的混频器更新），本移植缺这个。
- 症状是冲激响应被涂抹：跟踪器的时延扩展读数 **103 个 IR bin（10.6 ms）**，对参考的 0.8 ms，
  频率 Wiener 拿到约 5 倍过宽的长度，均衡被破坏（旧的极限环诊断是误导；那个扩展不是真的）。

**修复（本轮落地）**：`sync::freqtrack`（移植 DecDRM 的 `freq_delta_hz` + 粗 `sro_estimate`）加
`Nco::set_offset`，由 `rows_and_syms_tracked`（跟踪链路）和 `DrmReceiver::run()` 使用——流式 NCO
每符号重调。时域纠正后 live30 的 FAC MER 为 **16.3 dB**（参考 17.8，±2 dB 带内）、时延扩展
**0.62 ms**（参考 0.8 ms），极限环消失，跨次运行确定。纯 FFT 后的单元旋转不够——它去不掉
偏移已经烙进解调后单元的载波间干扰。

**接收机端到端解 live30**：整个抓取走接收机（粗捕获、流式频率跟踪、信道估计、FAC/SDC/MSC、
音频解帧）读得 FAC **64 ok / 10 bad**、台名 `SAN90 DRM BENCH`、HE-AAC 单声道 12 kHz、**240 个
解帧音频接入单元**——无需幅度归一化。验收测试（`receiver_on_live_capture`）全部钉住。

**wasm ABI 交换已尝试并回退**：`drm2::DrPlugin`（drm2 `DrmReceiver` 的
`DigitalDemodulator` 包装，加了 `buffered()`/`snr_db()`/`occupancy()` 供读数）已换入
`digital/mod.rs`，接收机的 `run()` 也改成了增量（模式检测一次，之后只解调新样本），修好了
块馈台面的超时。但 wasm 里 FAC 解码完全失败——**FAC ok 0 / err 约 74**，两条信道估计路径
都一样——而同样的代码原生在同一段抓取上读得 64 ok / 10 bad。这个 wasm 与原生的差异（两条
路径都失败，所以不是 time-Wiener）需要调试；分派已回退到前一个接收机让 `make ci` 保持
绿色，drm2 接收机保留其原生端到端验证。

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
| HE-AAC | coding 0，SBR 开，单声道 | `codec = "he-aac"` | FDK TT_DRM | 台面夹具和 wasm 夹具均可解出非静音 PCM，24 kHz 输出已验证 |
| HE-AAC v2 | coding 0，SBR 开，立体声 | `codec = "he-aac-v2"` + `stereo` | FDK TT_DRM | 已在台面验证：153 600 个 24 kHz 立体声样本，左右皆 1 kHz |
| xHE-AAC | coding 3 | `codec = "xhe-aac"` | FDK TT_DRM + SDC DRM Static Config | 已通过提交的 xHE 台面夹具走完整接收链，102 400 个非静音 24 kHz 样本 |

仍有两个夹具边界：

- 合成 AAC 夹具的 SDC 声称 SBR，但负载不含 SBR 数据，因此它是 FDK 安全性冒烟夹具，不是符合标准的 HE-AAC 广播；提交的台面码流是符合标准的，并已通过同一 FDK DRM transport。
- xHE 接收路径使用 FDK `TT_DRM` 和 SDC Static Config；libxaac 仍用于独立 USAC 编码/冒烟测试，其 MP4 模式解码器不接受紧凑 DRM Static Config。

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
| `drm2::chanest` —— 导频格点、时间插值、频率维线性插值、FAC 判决 MER | 进行中 | **干净夹具全链路 FAC MER 达 42 dB**（捕获、NCO、定时、解调、估计、均衡），而旧链的逐符号均衡器把同样信元落在距 4-QAM 点约 9° 以内。此前的负 MER 出在 Wiener 的 tap 相位而非结构：线性路径把星座精确落点，因此移除 Wiener（其 Levinson 求解器保留在 `dsp/` 并带测试），作为后续精化，以"与旧均衡器对照"为验收。未决：实时捕获最好只有 **+3.7 dB**，参考为 17.8 dB。对残余载波偏移做的一维扫描（定时固定，±6 Hz、0.5 Hz 步进，`sweep_residual_offset_with_timing_fixed`）最高也只到 3.7 dB 且曲线抖动——**没有任何纯频偏能把它救到可用**，所以缺口不在频偏。另两个扫描排除了其余嫌疑：按 -200~+200 ppm 的假定源速率误差重采样，MER 仍在 -4.8~-1.1 dB（`sweep_sample_rate_error_on_the_live_capture`）；帧相位也没有一个突出（`sweep_frame_phase_on_the_live_capture`，-7~+2.8 dB）。剩下的、也是数字指向的，是**频率维信道估计**：参考在这段捕获上测得约 1 ms 的时延扩展，其相干带宽（约 160 Hz）窄于散射导频的 281 Hz 格点间距（6 个载波），所以格点间线性插值跟不住信道。这正是 Dream 的频率维 Wiener 的用途；估计器的线性兜底只在干净平坦的夹具上达到 42 dB。在修好的格点索引上重新应用 Wiener 并在干净夹具上实测：时延扩展取 guard 比（0.25）、相位距离按格点步长时为 17.1 dB；把相位项符号翻转后升到 23.0 dB；取近平坦扩展（0.02）时为 18.2 dB。线性路径的 42 dB 仍然更优——说明剩下的未知既不是尺度也不是符号，而是 **tap 相位约定本身**：平坦模型把窗口内平均、丢掉了定时偏移在载波间的线性相位，而现有公式的两种符号都跟不住它。下一次应**逐行**核对参考的 `chanest/time_wiener.rs` 与 `chanest/mod.rs` 的 tap 应用（`len_ratio`/`offs_ratio` 如何进入 `arg`、以什么单位），而不是在参数空间里搜；并且应使用冲激响应跟踪测得的时延扩展而非常数。该实验记录在此以免重做；分支保持在**已验证**的线性路径（干净夹具 42 dB）。下一步：Dream 的 `TimeWiener`（按多普勒自适应的时域 Wiener）与冲激响应跟踪（时延/多普勒/SRO），这是本任务另一半。**这里记录一次时间维 Wiener 的尝试，以免原样重做**：因果滤波（Dream 的高斯多普勒相关 `R(tau)=exp(-2pi^2 sigma^2 Ts^2 tau^2)`、滤波长度 A5/B7/C9/D9、sigma 0.05 Hz、Levinson tap、最新样本优先次序）在两个捕获上都**更差**（干净 42.4 → 25.0 dB，实时 -1.4 → -8.9 dB），已回退。下一次应更贴近参考的 `time_wiener.rs`：按相位设计并**带上频偏相位项**、**对称窗 + 输出延迟**、以及用时间相关估计多普勒（其 60 秒时间常数）而不是固定 sigma。 |
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

## Wiener tap 公式（逐行读参考得出）

参考的 `chanest/mod.rs::update_freq_wiener`（第 220-235 行）回答了参数搜索无法回答的问题，并指出了上文中那次尝试的两处错误：

- `rhp[i] = sinc((i*x - diff) * len_ratio)` 与 `arg = PI * (i*x - diff) * (len_ratio + 2*offs_ratio)`
  —— 其中 `i*x` 与 `diff` 的单位是**载波**而不是格点步长（那次尝试用了格点单位，差了 `x` = 6 倍）。
- `len_ratio` 是"冲激响应长度 / 有用符号长度"，参考由跟踪给出（`pds_len / n_car`；这段捕获约 1 ms，即 ≈0.047）。
  那次尝试用的是 guard 比 0.25 —— 大了五倍，而且根本不是那个物理量。

因此只需改一个函数：`rhp` 与 `arg` 用**载波**单位，时延扩展取约 0.05（有跟踪后取实测值）而不是 guard 比。
改完先用干净夹具 ≥40 dB 这道门禁，再测实时捕获。

### 实测：仅靠参考公式并不能消除差距

忠实实现上面的公式（载波单位 + `len_ratio` = 0.05，且已修好格点索引）后，干净夹具仍只有 **17.1 dB**，
而线性路径是 42 dB。原因是结构性的，值得在下一次尝试前知道：时延扩展很小时相关矩阵接近奇异，
Levinson 解出的 tap 接近"对 11 个格点取平均"。平均正是**平坦**信道想要的，但夹具的定时偏移使信道成为
载波间的**线性相位斜坡**，跨 ±30 个载波平均会把它抹掉。参考之所以能用这种滤波，是因为它的
**定时/采样率跟踪把窗口对齐了**，因此信道没有残余斜坡；没有跟踪时，局部的两点插值是更好的估计器。
所以下一步是**跟踪**（Dream 的 `TimeSyncTrack` 与参考的 `track.rs`），频率维 Wiener 应在跟踪之后重测。

## 跟踪级：可以开工的状态

上面的测量说明必须先把跟踪落地、才能重测 Wiener；两个参考都定义了它，且常量一致：

- **功率时延谱（PDS）**：由信道估计的冲激响应得到，时间常数 `TICONST_PDS` = 0.25 s，
  能量门限 `CONT_PROP_ENERGY` = 0.02，最小统计门 `NUM_SAM_IR_FOR_MIN_STAT` = 10 帧、
  `OVER_EST_FACT_MIN_STAT` = 4.0。PDS 给出 `pds_len` 与 `pds_offset`——正是频率维 Wiener 需要的两个量
  （`len_ratio = pds_len / n_car`、`offs_ratio = pds_offset / n_car`）；它的**相位随时间的斜率给出采样率偏差（SRO）**。
- **SRO 跟踪**：先用 `SAM_OFF_ACQ_LEN_S` = 4 s（含 1 s 建立）捕获，之后 `SRO_STEP_S` = 0.1 s 一步、
  每步最多 `SRO_MAX_STEP_BINS` = 3 个 bin、速率 `SRO_TRACK_RATE` = 0.1、在 `SRO_TRACK_MIN_S` = 5 s 之后进入稳态，
  历史长度 `HIST_LEN_SAM_OFF_S` = 30 s。
- **定时跟踪**（Dream 的 `CTimeSyncTrack`）：在保护间隔能量剖面上搜索目标位置
  `TARGET_TI_POS_FRAC_GUARD_INT` = 9（保护间隔的十二分之九），接受判据为
  `TETA1_DIST_FROM_MAX_DB` = 20 dB、`TETA2_DIST_FROM_MIN_DB` = 23 dB 与连续性比例
  `CONT_PROP_IN_GUARD_INT` = 0.06 / `CONT_PROP_BEFORE_GUARD_INT` = 0.08；它的校正正是我们
  `TimeSync::timing_candidate` 当前代替的东西（外形已有：`LAMBDA_LOW_PASS_START` 0.99、
  `TIMING_BOUND_ABS` 150、`NUM_SYM_BEFORE_RESET` 5，但没有 SRO 输入）。
- **落位**：SRO 校正放在 NCO 与定时级之间（重采样或重相位）；PDS 供 `ChanEst` 的频率滤波用；
  定时校正调整 `TimeSync` 已输出的符号窗栅格。参考的 `track.rs` 把三者放在一个 `PdsTracker` 里；
  Dream 则分成 `CTimeSyncTrack` 与 `CTrack`。

### 低信噪比对比（task-4 的验收项之一，现在即可测）

`chanest::low_snr_tests::is_not_worse_than_the_previous_equaliser_at_low_snr` 向夹具加入确定性 AWGN
（带内 SNR 约 12 dB），并用两种估计器分别读 FAC MER：新估计器 **20.2 dB**，旧链的逐符号线性均衡器 **18.8 dB**。
增益来自"跨 3 符号的时间插值对导频噪声的平均"，这是相对旧实现的第一项**量化**的低电平改善。
测试断言新估计器不得落后旧实现超过 1 dB，因此此处的回归会让套件失败。

### 1 毫秒回波（task-4 的"衰落"验收项）

`chanest::fading_tests::is_not_worse_than_the_previous_equaliser_with_a_one_ms_echo` 在直达径后 1 毫秒加一条回波
——正是参考在台面捕获上测得的时延扩展——并用两种估计器读 FAC MER：新估计器 **14.9 dB**，旧链 **10.9 dB**。
在真实信号呈现的频率选择性条件下领先 4 dB，同样由测试守着。task-4 的"低电平"与"衰落"两项验收现在都有量化基线；
跟踪与 Wiener 的工作必须保持或改善它们。

### 模式 E：两个项目里都没有参考

两个 checkout 的检索都确认了这一点：Dream 的 `NUM_ROBUSTNESS_MODES` 是 4、`Parameter.h` 里没有模式 E 几何；
DecDRM（含发射端）也没有定义 `RobustnessMode::E`（在 `decdrm-core` 与 `decdrm-station` 里检索
`RobustnessMode::E` / `RM_ROBUSTNESS_MODE_E` / `DRM+` 均无结果）。因此模式 E / DRM+（VHF 变体）
无法从手头的参考移植；它的 OFDM 几何、帧结构与信令需要 ETSI ES 201 980 V4（或其他接收机实现）。
task-3/4 的其余部分都有参考支撑。

## 模式 E / DRM+（VHF）：范围与状态

**已实现并按规范钉住**：模式 E 几何从 ETSI ES 201 980 V4.2.1（§8.2 表 47、§8.3 表 49/50、
§8.4 表 57/58/60/61、§8.5 表 62–66）转写，并与 gr-drm 的 `drm_config.cc` 交叉核对：96 kHz
基带、FFT 216、保护 24、符号 240、每 100 ms 帧 40 符号、每超帧 4 帧、K −106..106（仅 SO 0）、
244 个 FAC 单元、21 个时间参考导频、无频率参考导频、散布导频每 4 载波逐符号移 1、符号 4 与
39 共 54 个 AFS 单元。测试钉住全部：`mode_e_geometry_matches_the_spec`（params）、
`mode_e_layout_matches_the_spec` 与 `mode_e_afs_phases_match_table_61`（cellmap），以及
`fac_cell_count`/`fac_positions`/`time_pilots`/`scattered_pilots` 的模式 E 分支（tables）。FAC/SDC/MSC 与音频解码已由仓库内确定性模式 E 夹具验证（见下文）；外部模式 E 信号源测试不在本目标范围。

**已实现并在合成模式 E 信号上独立验证**：模式感知 FAC 使用 116 个信息位、4-QAM 1/4 码率、两组业务描述，以及 RM flag 与四帧 identity/toggle 序列。SDC 使用模式 E 的 4-QAM 1/2 或 1/4 码率；MSC 使用模式 E 的 4/16-QAM 码率表、深度 6 的信元交织、每超帧 29 842 个有效 MSC 信元、每复用帧 7 460 个信元。信元映射中的 AFS-only 信元已从 SDC/MSC 数据中排除。native 测试生成零噪声 96 kHz 模式 E 信号，包含 FAC、SDC、四帧 MSC 和成对的 200 ms AAC 音频；接收机解出 FAC 17/0、SDC 4、7 个逐位一致的 MSC 帧和 15 个 AAC AU。提交的 `drm_modeE_so0_96k_aac.f32` 还通过 `drmplus` wasm ABI，在 48 kHz 和 44.1 kHz 输出设置下产出非静音 PCM。注入 ±100 Hz 载波偏移后，基于保护相关相位的跟踪仍能恢复 FAC/SDC/MSC/AU。

**入口已实现**：插件清单把 `drmplus` 与 DRM30 分开暴露。选择它会把信道化余量设为 100 kHz、以 96 kHz 构建接收机、高亮 ±50 kHz DRM+ 信道，并使用相同的解码音频通路；选择 `drm` 保留 48 kS/s DRM30 核心，从首次通过 CRC 的 FAC 自动选出广播占用，而不继续固定 SO3 信元表。Filter 控件将两种数字信道标为自动，不提供无法改变广播占用的模拟 IF 选项。

**外部信号源缺口（按用户决定暂缓）**：当前没有可用的真实 VHF Mode E IQ/WAV 录音。`Opendigitalradio/qt-drmplus` 引用的 `samples/02_drm_testeE.iq192` 在当前源码树及完整 git 历史均不存在。`kit-cel/gr-drm` 虽包含 DRM+ GNU Radio 发射 flowgraph，但已暂停编译及外部合成信号测试；本地依赖探查未生成独立发射器录音。真实 VHF 空口和外部 Mode E 源均不是本目标的验收门禁。继续使用仓库提交的 ETSI 规范驱动夹具及 native/wasm 检查，但不将其表述为独立的空口验证。两个外部项目的源码均未复制进本仓库。

**当前适用范围**：DRM+（模式 E）属于 VHF 波段 I/II（47–108 MHz），使用 96 kHz 基带及 AAC/xHE-AAC 音频。FAC/SDC/MSC 与音频链由仓库内确定性夹具测试；外部发射器与空口验证按本目标暂缓。

## 音频通路调试与可复用的离线数据（2026-10-05）

浏览器/实时 DRM 音频曾“一秒一断”并最终停止。以下修复已有离线测试覆盖，实时播放仍待验证：

- `websa_dsp_drm_audio_pcm` 把接收机整个缓冲区取走，却只拷贝固定大小的输出块（32768 样本），每个音频突发的尾部（38.4 kHz 下约 30%）被丢掉。已修：新增 `DigitalDemodulator::drain_audio_pcm(limit)`，未取完的尾部保留（`drain_audio_pcm_keeps_the_tail`），worker 自身上限也提高，突发不再被截断。
- 接收机在连续 12 次 FAC 失败**或 3 秒没有成功 FAC** 后自动重捕。完全静音时保护间隔同步不会产生符号，FAC 错误计数不变，旧锁会一直挂住；现在这种情况也能失锁。重捕清除旧台名、SDC/音频/MSC 和解码器状态，只保留最近 1.5 秒基带；原先保留约 4 秒会在静音通道上重放旧信号并再次误锁。台面夹具接静音再接夹具的定向回归已验证失锁与重新捕获；实时验证仍待完成。
- wasm 长期开台时，原始 IQ、已处理 OFDM 行及 FAC/MSC/AU 历史现在都有保留上限，同时维持累计读数和 AAC 解码游标。FDK 的 HE-AAC v2 交错立体声先下混再进入单声道 worklet 的重采样，避免播放时长与音高错误。native 分块/整段对照及 xHE、HE-AAC v2 的 wasm 夹具测试通过；本次没有做浏览器播放或真实电台 e2e。

- 前述修复后仍有周期断音：按 worker 分块喂入已提交的 wasm 夹具，PCM 突发依次为 0.8 秒、1.2 秒、1.2 秒，后两次相隔约 1.2 秒（`WEBSA_DRM_INCREMENTAL=1` 运行 `frontend/scripts/drm_live_audio.mjs`）。单声道 worklet 的 0.9 秒上限每次丢弃至少 0.3 秒，初始只预充 0.25 秒也会在下一突发前欠载。现在仅 DRM 使用 2.4 秒上限、等两次突发后以 1.6 秒门限开播；模拟的突发节奏测试无丢样/欠载，模拟模式维持原设置。**实时台面听感仍待复测**。

- DRM 读数与 FAC 星座图现在显示在结构化的 DRM 浮窗里，不再被误当成 FT8 表格行；FT8 专用解码器及第二条 IQ WebSocket 只在 FT8 模式启用，切换时丢弃迟到的跨模式消息。普通 DRM 读数不再为递增的符号数逐块刷新。定向的模式切换与界面测试通过。用户反馈 worklet 修复后声音似乎连续；Pluto 文件循环发射时偶发 FAC 错误，而强信号真实电台似乎无错误。循环边界尚未独立测量，不能以此调整真实信号解码器。

- DRM 浮窗在默认 460×240 大小内同时呈现台名、模式、实测 FAC MER、由 FAC 判决误差估计的 SNR、FAC 正确/错误、MSC/音频计数与紧凑星座图。SNR 按平均载波功率及占用带宽校正，属于判决反馈估计值，不是标定过的射频噪声底测量；完整 FAC 帧出现前显示暂无。提交的 HE-AAC 夹具在 wasm ABI 上读得 MER 19.9 dB、SNR 21 dB。调谐或 IQ 流复位会立即清空旧台名/星座，空频点也不残留；暗色/亮色及窄窗截图已检查。
- DRM30 先以 SO3 完成模式/FAC 捕获，再按首个 CRC 正确的 FAC 切换到电台声明的占用带宽，然后组装 SDC/MSC。提交的模式 B SO0（4.5 kHz）和 SO5（20 kHz）发射夹具在整段及 3248 样本分块两种输入下分别解出 FAC 13/0、SDC 4、MSC 5 和 xHE-AAC AU；原 SO3 与 A/C/D 夹具仍通过。原 Filter 的 0.5–180 kHz 模拟按钮不选择 DRM 占用：窄带档的后端 DDC 仍约 48.8 kS/s。选择 DRM 时设定 12 kHz DDC 几何（输出至少 48 kS/s，足以容纳 20 kHz 信道），隐藏模拟手选并标示 FAC 自动识别；频谱高亮在锁定前显示 20 kHz 搜索范围，锁定后随 FAC 带宽更新，不再误用 FT8 的 +100..3000 Hz。DRM30 核心固定 48 kS/s，音频 PCM 仍按声卡 44.1/48 kHz 输出。其余模式/占用组合有信元表，但尚无各自提交的发射夹具。
- 用当前 wasm ABI 对 `/tmp/cnr_rec.f32` 逐块复查：正常区间 32 kHz xHE 平均每约 1.2 秒输出 1.203 秒 PCM，但第 564 块到 775 块之间约 14 秒没有 PCM，随后 FAC 计数由 93 重新从低值开始，符合重新捕获。该抓取不足以证明更短的听感拼接只由编解码器引起；FAC 正确/错误数会在重捕时归零，当前 `err 0` 也不代表此前 MSC/PCM 连续。本次没有进行设备实时 e2e。

**下一轮可复用的可靠离线数据**：一条已知音频的台面环路——`cowtts` 合成语音到 `/tmp/cowtts_test.wav`，`decdrm tx` 生成 `/home/hui/drm-bench/drm_iq_tts.wav`（xHE-AAC 单声道 24 kHz），`tools/pluto_drm_tx.py` 发射，`tools/drm_capture.py` 抓基带。在这条环路上，我们的接收机和参考都产出干净连续的音频（`/tmp/tts_rec.f32`、`/tmp/ours_tts_24k.wav`、`/tmp/decdrm_tts.wav`），因此**解码器是清白的**。而在真实 13.825 MHz 电台（CNR-1，xHE-AAC 32 kHz，`/tmp/cnr_rec.f32`）上，**两个解码器都有相同的拼接伪影**，说明伪影在真实信号或采集链路（HF 衰落和/或采集路径），不在解码器；参考在该抓取上解出更多帧（725 对 536），是下一个鲁棒性目标。已提交的夹具（`drm_live_modeB_so3_48828.f32`、模式 A/C/D、xHE、HE-AACv2 抓取）仍是确定性回归集。
