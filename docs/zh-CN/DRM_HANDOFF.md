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

## 环境注意

- 负载高时后端会卡住（日志里的 “SDR stream stalled (watchdog)”、65 ms 的采集步进）。台面测试前
  用 `./stop.sh && ./run.sh` 重启服务。
- 本机上无头 Chromium 会崩溃（`chrome-headless-shell` 在合成器里 trap），UI 探针请用 Playwright
  的 Firefox。`tools/e2e/demod_switch.py` 仍在用 Chromium。
- 启动 Pluto 发射机请用子 shell（`( nohup ... & )`）；把 `&` 与 `&&` 串在同一行里已经多次静默
  失败。
