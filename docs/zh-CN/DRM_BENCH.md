# DRM 台面环回（PlutoSDR → SAN-90）——实测记录

本页记录用真实信号测试 DRM 接收链的台面环境。页内给出命令、实测数值和未解决问题。
用它可以不依赖短波广播而复现故障。

## 为什么要台面环回

实验室收不到短波 DRM 广播。短波传播条件每小时都在变。台面环回用电缆或短距离
代替传播路径。这样信号可重复，内容也已知。

## 台面配置

收发相距 2 米。PlutoSDR 发射，SAN-90 用外接天线接收。

- AD9363 发射机不能调谐到 HF。它的下限约为 325 MHz。这不是限制：DRM 信号就是
  OFDM 波形，载频对解码器没有意义。因此在 400 MHz 发射。
- DRM 基带位于发射机本振的 `+100 kHz`。这样直接变频泄漏会避开信号。把分析仪
  调到「本振 + 100 kHz」。
- 分析仪设置：SDR 模式、中心 400.1 MHz、IQ 采集带宽 195 kHz（decimate 256）、
  参考电平 -40 dBm。

## 产生并发射 DRM 信号

按顺序执行以下命令。

```
# 1. 生成 DRM IQ 文件（在 DecDRM 仓库中执行）。
decdrm tx tools/bench/drm_bench/station_iq.toml --output drm_iq_15s.wav --duration 15

# 2. 由 PlutoSDR 发射。也可用统一入口：
#    python3 tools/pluto_tx.py drm --iq drm_iq_15s.wav --seconds 600
python3 tools/pluto/pluto_drm_tx.py --iq drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

本次台面有两条经验：

- 不要发射 60 秒文件。该文件会产生 250 MB 的循环缓冲。Pluto 拒绝这次推送并报
  `OSError: [Errno 14] Bad address`，之后电台保持静默。用 15 秒文件（63 MB）可行。
- 调试接收机前先看频谱。发射机静默与接收机损坏在 DRM 读数上表现相同。信号在
  400.1 MHz 表现为约 10 kHz 宽的平顶。本次 2 米距离下峰值约 -110 dBm。

## 抓取浏览器收到的基带

DRM 接收机在浏览器中运行。它从 `?iq=1` WebSocket 读取信道化基带。抓取工具订阅
同一条流，所以文件就是解码器的确切输入。

```
python3 tools/bench/drm_capture.py --seconds 10 \
    --out tests/fixtures/drm/drm_live_modeB_so3_48828.f32 --json /tmp/drm_live.json
```

工具先丢弃 1 秒再计数。若一开始就计数，会把后端在 socket 建立期间排队的帧算进去。
这段突发会把样本数放大约 1.8 倍，速率看起来就不对。

## 实测数据（2026-10-03，单次台面）

| 项目 | 数值 | 测量方式 |
| --- | --- | --- |
| 帧头中的基带速率 | 48828.125 Hz | IQBF 帧的 `rate` 字段 |
| 真实基带速率 | 48833.85 Hz | 循环前缀周期：每 1024 点有用部分对应 1041.79 点 |
| 帧头速率精度 | 偏低 0.012 % | 由上两行得出 |
| 电平 | rms +15.6 dBFS，峰值 +29.2 dBFS | 满刻度为 1.0，即约 30 倍满刻度 |
| 占用带宽 | 9.82 kHz | 按公布的速率计算，说明是 DRM mode B / 10 kHz |
| 投递 | 50.1k 样本/秒，每帧 66.5 ms | 记录 40 帧的到达时刻 |
| 内容重复 | 无 | 没有与相邻帧完全相同的帧 |

## 对照验证：Dream 能解同一批样本

另一个 worktree（`feature/drm-dream-decoder`）运行 Dream 解码器。Dream 是本台面的
对照基准：它只回答一个问题——这段台面信号本身能否解码？

```
python3 tools/analysis/drm_oracle_check.py tests/fixtures/drm/drm_live_modeB_so3_48828.f32 48828.125
```

结果：

```
messages=25  snr=20.9 dB  robustness=1  bandwidth=10.0 kHz
status={'io': 0, 'time': 0, 'frame': 0, 'fac': 0, 'sdc': 0, 'msc': 0}
service: label='SAN90 DRM BENCH' bitrate=20.96 audio_mode=Mono protection=EEP
```

Dream 解出了元数据和全部信道。因此台面信号是好的，出问题的是本分支的接收链。

## 接收链在抓取基带上的表现

`cargo test --release --test drm_live_fixture` 运行下表检查。

| 输入 | 结果 |
| --- | --- |
| 按投递速率直接输入（48828.125 Hz 样本进 48 kHz 内核） | 不锁定，`timesync::acquire` 返回 `None` |
| 重采样到 48 kHz 后输入 | 锁定，mode B，占用 SO3，FAC SNR -18.9 dB，14/14 个 FAC 块 CRC 失败，无台名，无 MSC 帧 |
| 重采样并去除载波频偏后 | FAC SNR -13.6 dB，FAC 仍然失败 |
| `tests/fixtures/drm/drm_modeB_so3_48k.f32`（合成基准） | FAC SNR 36.6 dB，0 个 FAC 错误，5 个 SDC 块通过，台名解出 |
| 基准 + 15 dB SNR 白噪声 | 可解，FAC SNR 26.2 dB |
| 基准 + 200 us、-6 dB 单径回波 | 可解，FAC SNR 21.7 dB |
| 基准放大到 400 倍 | 可解，FAC SNR 36.6 dB |

后三行排除了电平、噪声和多径三个原因。真实信号约 21 dB SNR，而基准在 15 dB 仍可解。

## 已排除的因素

以下因素都用抓取的真实文件测过，都不能解释该故障。

- **电平与缩放**：基准夹具放大 400 倍仍可解；真实文件缩到 1/256 结果不变。
- **载波频偏**：接收机现已去除。见下一节。
- **帧相位**：在去除频偏的前提下强制测试了 0..14 的全部候选。真实抓取上没有候选能解出 FAC。
- **整载波锚点**：在去除小数频偏的前提下，扫描了 -6 到 +6 个载波间隔的全部搬移。没有
  任何搬移能解出 FAC。
- **重采样质量与速率精度**：用窗 sinc 按公布速率和实测速率重采样，表现与管线里的线性
  重采样相同；连续两次重采样也没有变化。
- **削顶**：把夹具硬削顶到其 rms 的 1.0 倍后仍能解码，所以信号过强不是原因。
- **频谱方向**：取共轭后的表现相同。
- **直流分量**：文件均值 0.002，去掉后结果不变。
- **采样率**：实测速率 48833.85 Hz，按该精确速率重采样结果不变。

## 载波频偏：FAC 失败的原因

接收机不做载波频偏校正。它假设信号正好落在 DRM 载波栅格上。真实信号不是这样。

在真实抓取上的测量结果：

- 载波频偏为 +19.7 Hz。该值由循环前缀相位得出，整段抓取内波动不超过 0.8 Hz。
  同一方法在合成夹具上得到 0.0 Hz。
- +19.7 Hz 是 DRM 载波间隔（46.875 Hz）的 42 %。此时每个载波都落在两个 FFT 频点之间。
- DRM 内核只能容忍几 Hz。下表把频偏加到合成夹具上，再运行接收机。

| 加到夹具上的频偏 | FAC SNR | FAC 结果 |
| --- | --- | --- |
| 0 Hz | 36.6 dB | 15 个块全部通过 |
| +5 Hz | 12.6 dB | 15 个块全部通过 |
| +10 Hz | 6.3 dB | 15 个块全部通过，SDC 失败 |
| +20 Hz | -1.7 dB | 15 个块全部失败 |
| +23.4 Hz（半个载波间隔） | -14.4 dB | 14 个块失败 |

真实抓取的表现相当于夹具在约 +20 Hz 时的情况。因此 FAC 失败是载波频偏问题，不是信道、
电平或噪声问题。

接收机现在从保护间隔相关中估计频偏，并在 FFT 之前去除它。`timesync::acquire` 通过
`Acquired::freq_offset_hz` 给出该值，`DrmReceiver::remove_carrier_offset` 应用校正。
在 48 kHz 下估计范围无模糊区间为 `+/- fs/(2*nu)`，约正负 23 Hz。

该修复已在夹具上验证：注入 -19.7、-10、-5、+5、+10、+19.7 Hz 的频偏后，FAC 无错误且
台名正确（`wasm/tests/drm_phy_robustness.rs::removes_a_carrier_offset_before_the_fft`）。
修复前，这些频偏从约 +20 Hz 起就会失败。

真实抓取在校正后仍然 FAC 失败（-13.6 dB）。因此载波频偏是必要条件，但不是唯一原因。
剩余缺陷位于真实信号的信元提取环节，下面的已排除清单记录了它不是什么。

## 未解决的问题

1. **速率**：DRM 内核固定工作在 48 kHz（`wasm/src/digital/drm/params.rs`）。信道化
   器投递 48828.125 Hz。不做重采样就无法捕获。管线本来有重采样器，worker 必须把
   真实速率传给它。
2. **载波锚点与超帧相位**：已修复。接收机从检测到的带边推出整载波部分，从保护间隔相关
   推出小数部分；并逐个尝试超帧相位，取 SDC CRC 通过的哪一个。真实抓取现在能锁定，解出
   FAC 与 SDC，显示台名、鲁棒模式、带宽、码率、编码和 FAC SNR，并通过 MSC 校验。
   `wasm/tests/drm_live_fixture.rs` 覆盖了这些。
3. **真实码流的音频解码**：已解决（2026-10-04）。台面码流——标准的 DRM HE-AAC 配置：
   12 kHz 核心 + SBR，每帧 400 ms 五个 208 字节接入单元——在 wasm 模块内解码出非静音的
   24 kHz PCM：整段推送得到 76 800 个样本，worker 的 3 248 样本分块喂入得到 48 000 个。
   共三个根因，全部修复：

   - **对齐分配器没有清零内存。** FDK 自己的 `genericStds` 用 `FDKcalloc`（malloc 并
     清零）支撑 `FDKaalloc`/`FDKaalloc_L`，因为它分配的持久通道信息在写满之前就会被读
     （HCR 边信息排序先读后写）。我们的 shim 用的是普通 `malloc`，内存里是堆上的残渣。
     在新鲜的 wasm 堆上残渣恰好是零，解码通过；在回收过的堆上是垃圾，HCR 解码器把垃圾
     变成越界下标，而 wasm 的精确边界检查把它变成 trap。这就是「依赖堆布局」的全部
     原因。证据来自同一批 FDK 源码的 native 构建加 MemorySanitizer：
     `aacdec_hcr.cpp:782` 的 `HcrSortCodebookAndNumCodewordInSection` 读到
     `use-of-uninitialized-value`，内存由 `CAacDecoder_Init` 里的 `FDKaalloc_L` 分配。
     shim 现已照搬 FDK 语义：calloc、向上对齐、原始指针存在返回地址前一个字供
     `FDKafree` 使用。
   - **不再剥离 SBR 标志。** 分配器修好后，FDK 能解出真实的 SBR 负载（按 DRM 语法逐位
     反序存在每个接入单元末尾）并以 SBR 速率输出。读数也报告 SBR 速率（核心 x 2），这
     把合成夹具的期望速率从 24 000 改到 48 000 Hz——它的 SDC 在 24 kHz 核心上声称 SBR，
     但负载里没有 SBR 数据。
   - **两个流式缺陷让后锁定通路一直静默**（整段推送都看不见）：MSC 的
     `CellDeinterleaver` 每次 pass 都重建，长交织器的五帧填充把整个 3 秒窗口耗光——现在
     它跨 pass 持久化，帧带绝对序号，重叠窗口不会重复喂数；`remove_carrier_offset`
     覆盖而不是累加 `mix_w`，整载波锚点校正之后，锁定后到达的每个样本都少转了第一次
     校正的那一份（约 120 Hz 的分数部分），pass 的 SNR 逐帧从 20 dB 跌到 1.7 dB。整段
     推送看不到它：锁定那一次把整个缓冲区一次性原位旋转，锁定后没有新样本到达。

   决定性的是第四个根因，通过把接入单元与参考接收机**逐字节对比**找到：**DRM 文本消息
   被留在了音频 super frame 里。** 当 SDC 音频描述符置了 text 标志（我们的码流就是），每个
   音频逻辑帧的**最后 4 字节是文本消息**，不是音频。我们的最后一个接入单元把它们吞了进去，
   于是 FDK 从帧尾**反向**读取的 SBR 载荷整体偏移 4 字节，FDK 用确定性噪声填补缺失的 SBR——
   听众听到的"沙沙"正是它；这也解释了为什么只解核心（清掉 SBR 标志）是干净单音、完整
   HE-AAC 解码就不是。现在我们的接入单元与参考逐字节一致（对齐后 34/35 完全相同），解出的
   音频就是台面发射机的 1 kHz 单音（主频 1000.0 Hz，5 kHz 以上能量占比 0.002）。参照实现
   的做法：Dream 的 `AACSuperFrame`/`CDataDecoder` 与 DecDRM 的 `split_text_message` 都会
   在解帧前剥掉这 4 字节；`wasm/src/digital/drm/audio.rs::split_text_message` 现在照做。

   还有一个交付缺陷：worker 的音频读取是"带偏移量的头部窗口"，一旦接收器缓冲超过上限
   （约 1.4 秒音频）就再也读不到新数据。现在 ABI 改为**排空语义**：
   `websa_dsp_drm_audio_pcm` 返回上次调用以来新增的部分，worker 全部转发，接收器缓冲保持
   有界。

   回归门：`live_capture_streams_audio_after_the_lock`（native，分块喂入）、
   `audio::tests::text_message_is_not_part_of_the_audio_super_frame`（那 4 字节）、以及
   `frontend/src/__tests__/drmLiveAudio.test.ts`（wasm：24 kHz 非静音 PCM 且峰值在 1 kHz）。
   字节级交叉验证可复用：`wasm/examples/dump_drm_au.rs` 导出接入单元，DecDRM 的
   `crates/decdrm-codecs/examples/decode_au_dump.rs` 解码它们。
3. **超帧相位**：`DrmReceiver::decode` 假设缓冲区从超帧符号 0 开始。真实抓取从任意
   位置开始。因此 SDC 与 MSC 取到了错误的信元。
4. **超帧断言**：不完整的超帧会让 SDC 信元数触发断言
   （`wasm/src/digital/drm/fec/mlc.rs:335`）。wasm 内的断言可能让模块 trap，并破坏
   同一会话后续所有模式。
5. **读数不透明**：在台名解出之前，解码窗口什么都不显示。操作员看不到锁定状态和
   FAC SNR，于是「已锁定」与「接收机已死」无法区分。

## 模式切换回归：首次观察

现象报告是：用过 DRM 之后切到其他解调模式就没有声音，刷新页面才能恢复，preset 也无效。
第一次台面尝试没有复现。把这次尝试记录下来，因为触发条件很关键。

使用的步骤（Chromium、全新页面、服务在 8080 端口）：

1. 用 `SET_MODE` 切到 SDR 模式。
2. 打开页面并把 SDR 音频开关打开。
3. 在解调器组中选择 DRM，并打开解码窗口。等待 12 秒。
4. 选择 AM，等待 8 秒。

观察结果：DRM 读数一直为空（在该采样率下没有锁定），而 AM 立刻恢复出声
（`dsp_pcm_blocks` 由 0 变 241，`dsp_pcm_rms` 0.05..0.08，`dsp_ratio` 1.0005）。
切换是正常的。

原因后来找到了：wasm 的 DRM 音频解码在台面码流上会 trap，而 trap 会杀掉 worker 实例。
页面随后持有的是已死 worker 的引用，所以之后每次切换解调模式都没有反应，只有刷新页面才行。
`worker.onerror` 现在会丢弃该 worker，下一次切换模式会新建一个
（`frontend/src/__tests__/workerRecovery.test.ts`；修复前该测试失败）。

还有两个条件需要测试，而且很可能都关键：

- **浏览器内真正的 DRM 锁定。** 在当前速率下 DRM 内核根本走不到解码路径。该路径上的
  断言（`wasm/src/digital/drm/fec/mlc.rs:335`）可能让 wasm 模块 trap。模块一旦 trap，
  同一会话后续所有模式都会失效，这与报告完全吻合。先修好速率，再重试该序列。
- **首次选择 DRM 前不碰音频开关。** 上面的步骤提前打开了音频。去掉这一步重试，并把
  点击解调器改成切换 preset。

## 复现本次台面

```
# 1. 启动服务并确认设备。
./status.sh

# 2. 发射 DRM 信号（见上文），并在频谱上确认信号。

# 3. 把分析仪调到台面信号。
#    SDR 模式、中心 400.1 MHz、decimate 256、listen 400.1 MHz、demod DRM、ref -40 dBm。

# 4. 抓取基带，然后离线解码。
python3 tools/bench/drm_capture.py --seconds 10 --out /tmp/live.f32
cd wasm && cargo test --release --test drm_live_fixture -- --nocapture

# 5. 或按 worker 的方式经 wasm 路径解码（非静音 PCM）。
cd frontend && node --experimental-strip-types scripts/drm_live_audio.mjs /tmp/live.f32

# 6. 浏览器端到端：面板选 DRM、看读数、切走再切回——全程不刷新页面
#   （故意用 Firefox；本机的 headless Chromium 会崩）。
python3 tools/e2e/drm_switch.py --url http://127.0.0.1:8080
```

## 台面实测（2026-10-04）：台面环路出音频了

修复音频缺陷的这次会话以全链路绿灯收尾：发射 `drm_iq_15s.wav`（TX gain -5 dB），
应用设 ref -40 dBm：

- 12 秒抓取 native 解出 35 个接入单元、22 个 FAC 块（au=35、facs=22）；文本消息修复后
  同一路径达到 au=70，FAC 错误数为零。
- 同一抓取经 wasm 分块路径（`scripts/drm_live_audio.mjs`）产出 230 400 个非静音的
  24 kHz PCM 样本，主频就是台面单音：1000.0 Hz，5 kHz 以上仅占 3% 能量（修复前完全不是
  单音，频段里几乎全是噪声）。
- 参考接收机（Dream 的 console 构建）能从同一发射信号解出音频，DecDRM 接收机写出干净的
  1 kHz 单音；与两者的字节对比正是找出文本消息缺陷的工具。
- `tools/e2e/drm_switch.py`：DRM 在浏览器内锁定（读数：`locked: B, 10 kHz ...`
  `station: SAN90 DRM BENCH`、`FAC SNR 12 dB`），切 AM 后基带继续流动，切回 DRM 再次
  锁定——全程无页面刷新。

本次会话还给上面的清单新增了两条台面事实：

- **DDC 的数字增益因服务实例而异。** 一次启动送来的抓取 rms +34 dBFS（削波，1.4 秒
  到处都能锁定）；下一次同样的 TX 增益却只有 -14 dBFS，native、wasm、浏览器全都锁
  不上。重启服务后热电平恢复。所以：台面上 FAC 解不出来时，先重抓一次、看抓取 JSON
  里的 `rms_dbfs`，再去怀疑接收机。
- **未锁定时 worker 可能饿死浏览器流。** 弱信号页面把浏览器基带冻在约 30 块（接收机
  每块都对整个缓冲区重跑捕获）。信号能锁定的同一页面则无限期运行。读 IQ 诊断里冻结
  的 `blocks=` 计数时记住这一点。
