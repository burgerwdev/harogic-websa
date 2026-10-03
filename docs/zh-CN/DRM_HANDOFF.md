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
- 测试：`cargo test --test drm_fixture` 4/4，真实抓取测试 4/4，物理层鲁棒性 5/5，前端 322 项
  通过（另有一个 FT8 速度测试只在机器繁忙时失败）。

未完成：

- 真实 HE-AAC 码流（12 kHz 核心 + SBR）的 AAC 音频还不能解码；接收机跳过该配置，避免一次
  故障拖垮整个会话。
- 读数不跟随信号变化：数值来自锁定那一次，因为接收机锁定后就不再接收基带。修复已经写好并
  在离线验证过，见下面第 3 步。

## 花了时间才确认的台面事实

- **参考电平 −45 dBm**：此时带内对比度 25 到 27 dB。−40 dBm 时只有 6 dB，无法锁定。
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

目标明确不把 Dream 子进程引入本分支。读这些项目是为了算法，而不是再加一个解码器。

## 下一步（按顺序）

1. 去掉 `decode_audio` 里的 SBR 保护，用 node 测试台把真实抓取喂进去（`instantiateDsp`、按 3248
   样本分块推送、读 `websa_dsp_drm_audio_pcm`）。第 3 条的填充可能已经顺带消除了实时链路的
   trap。如果 PCM 非零且非静音，音频通路就通了。
2. 如果仍然 trap，把第 2 条做完（落库对齐版 `FDKaalloc`，然后带符号、不做 strip 重建 FDK，定位
   出错函数），或者把故障限制在音频解码之内。
3. 落库读数刷新。该实现已写好并在离线验证过，只因为 FDK 故障导致不稳定而回退：常量
   `PASS_SECONDS` 与 `WINDOW_SECONDS`，字段 `locked_r`、`locked_super_phase`、`locked_map`、
   `locked_row_start`、`pass_at`、`audio_units_decoded`，去掉 `push` 的提前返回，
   `decode_next_window`，给 `decode` 增加 `row_base` 参数，以及 `clear_readout_state`。
4. 然后收口 task 4（preset 路径，台面确认）与 task 5（夹具、文档、一次完整 `make ci`）。

## 环境注意

- 负载高时后端会卡住（日志里的 “SDR stream stalled (watchdog)”、65 ms 的采集步进）。台面测试前
  用 `./stop.sh && ./run.sh` 重启服务。
- 本机上无头 Chromium 会崩溃（`chrome-headless-shell` 在合成器里 trap），UI 探针请用 Playwright
  的 Firefox。`tools/e2e/demod_switch.py` 仍在用 Chromium。
- 启动 Pluto 发射机请用子 shell（`( nohup ... & )`）；把 `&` 与 `&&` 串在同一行里已经多次静默
  失败。
