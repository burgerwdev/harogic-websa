# 通过 Dream 解码 DRM（后台解码器）

本页说明 DRM 接收模式：把**外部 Dream 接收机**作为后台进程运行，并将 SDR 管线的信道化复基带送入其中。
这是"复用现成解码器"的路线；`feature/drm-demod` 上的 Rust/WASM 重写是另一条独立工作，本工作不影响它。

## 隔离

本工作在自己的 worktree/分支中进行，绝不与另一个 pi 的 worktree
（`/home/hui/git/harogic-websa`，分支 `feature/drm-demod`，占用 8080/8099）共享端口或 IPC。

- 分支：`feature/drm-dream-decoder`（基于 `master`）。
- worktree：`/home/hui/git/harogic-websa-drm-dream`。
- Web 服务端口：`WEBSA_PORT=8180`；e2e/fake 端口：`WEBSA_FAKE_PORT=8199`。
- IPC：`/tmp/drm-dream-*`（状态套接字、sink），运行时按 PID 命名空间隔离。
- `tools/drm_dream/env.sh` 是这些约定的唯一定义处；运行前先 source 它。

## 解码器二进制

Dream 的 `--status-socket`（本集成所读取的按行 JSON 状态流）**不在**任何打包二进制中：

| 二进制 | 构建方式 | `--status-socket` |
| --- | --- | --- |
| `dream-nox` 2.2.4（AUR） | wwek fork，`CONFIG+=qtconsole` | 缺失（未编译进去） |
| `dream` 2.3（AUR） | 上游 `Drm-tools/dream`，CMake `USE_QT=ON` | 缺失 |

只有 wwek fork 的 console 构建（`qmake CONFIG+=console`）会启动 `CStatusBroadcast`。把它构建到
worktree 内（已被 git 忽略）：

```
tools/drm_dream/build_dream.sh          # -> tools/drm_dream/build/dream
```

可用 `DRM_DREAM_BIN` 覆盖路径。否则 `web_sa/measurements/sdr.py::_default_dream_bin()` 按能力解析解码器：
依次尝试 `DRM_DREAM_BIN`、`/usr/local/bin/dream`、`/usr/bin/dream`、最后是 worktree 内的构建，
选用第一个 `--help` 中带有 `--status-socket` 的二进制。因此已安装的二进制一旦具备该能力就会被自动使用。
完整理由（以及用户选择内置 console 构建的决定）见 `tools/drm_dream/DECISION.md`。

## 数据路径

```
SDR DDC (i, q, f_out ~= 48 kHz；f_out 用**实测值**，而非标称值)
  -> 低通 -> 线性重采样到 48 kHz -> DreamDecoder.feed()
  -> pacat --raw -> 私有 null sink -> dream -I <sink>.monitor -c 6 --sigsrate 48000
       |- --status-socket -> 按行 JSON -> state.sdr_drm
       `- -O <audio sink> -> parec -> 单声道 int16 -> AUDF 帧
```

重采样使用信道化器的**实测**输出速率（`_measure_baseband_rate`）。用标称速率会在 48 kHz 声卡上
留下缓慢漂移，接收机因此每秒左右重锁一次：改用实测速率后 `msc=0` 从约 50% 升到约 90%。

音频通过第二个私有 null sink 用 `parec` 实时采集。Dream 自带的 `-w/--writewav` 不可用
（接收机在解码时该文件仍为 0 字节）。

## 如何选择

在 SDR 面板的解调器组中选择 `DRM`（该模式属于后端/服务端模式，浏览器没有对应内核）。选中期间，
`STATUS.sdr.drm` 携带解码出的元数据，DRM 读数显示台名、鲁棒模式、带宽、码率、编码与同步状态。
音频通过常规的 SDR 音频开关播放。

## 台架发射机（PlutoSDR）

`tools/pluto_drm_tx.py` 用 PlutoSDR 循环发射一段 DRM 信号，方便不等短波传播就能测接收。

- AD9363 的 TX 发不了 HF（最低约 325 MHz），但 DRM 只是 OFDM 波形，载波频率对 Dream 无意义：
  在 UHF（默认 400 MHz）发射、SAN‑90 在 UHF 接收即可。用**同轴电缆 + 衰减器**从 Pluto TX 接到
  SAN‑90 射频口比空口辐射更好（电平可控、无传播、无干扰）。
- IQ 输入是 DecDRM 发射机输出的 int16 WAV（`decdrm tx station.toml --output drm_iq.wav
  --duration 60`，`format = "iq"`，默认 `iq_swap` 即 I 在左）。Dream 只认得 int16 WAV，float32 不行。
- DRM 基带放在 `--base-hz`（默认 +100 kHz），避开本振泄漏；应用调到 `LO + base_hz`。**不要**加
  `--conj`（共轭）：默认方向才是 Dream `-c 6` 期望的。
- 默认用循环 DMA 缓冲（`--stream` 才是分块单次推送）。循环回卷与 Pluto/SAN‑90 时钟偏差是台架环路上
  MSC 间歇出错的主要来源；真实空口信号没有这两个问题。

## 验证

```
source tools/drm_dream/env.sh
python3 tools/drm_dream/probe_fixture.py          # fixture -> Dream 元数据与 manifest 比对
python3 tools/drm_dream/loopback_probe.py         # 同样的链路，经 null-sink 回环
python3 -m pytest tests/test_drm_dream*.py tests/test_drm_state.py -q
DRM_DREAM_AUDIO_FIXTURE=/path/to/real.rec python3 -m pytest tests/test_drm_dream_audio.py -q
WEBSA_FAKE=1 WEBSA_PORT=8180 python3 -m web_sa.supervisor &   # 然后：
python3 tools/e2e/drm_mode.py --url http://127.0.0.1:8180
```

`WEBSA_DRM_DUMP=<path>`（环境变量）会把送进 Dream 的精确基带追加到 raw int16 文件，
用于排查解码问题时把实时喂流与抓下来的 sink 做对比。

## 已知限制

合成 fixture `tests/fixtures/drm/drm_modeB_so3_48k.f32`（来自 `feature/drm-demod`）能正确解出**元数据**，
但 Dream 将其 MSC 帧报告为 `CRC_ERROR` 并拒绝其音频：这是 DecDRM 发射机与 Dream 之间的互操作差异，
而非构建问题。因此音频采集用真实录音验证。

接真实天线时能收到真实 DRM 电台（元数据与音频），但信号弱时 MSC 只能短暂保持：xHE‑AAC 音频大约需要
`MER >= 15 dB`。Pluto 台架环路上 `msc=0` 约占 90%；剩余掉锁来自循环缓冲回卷与 Pluto/SAN‑90 时钟偏差，
并非应用本身。
