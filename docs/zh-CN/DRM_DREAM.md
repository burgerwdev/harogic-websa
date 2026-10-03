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

可用 `DRM_DREAM_BIN` 覆盖路径；否则 `web_sa/measurements/sdr.py` 使用 worktree 内的构建。

## 数据路径

```
SDR DDC (i, q, ddc.fs_out ~= 48 kHz)
  -> 低通 -> 线性重采样到 48 kHz -> DreamDecoder.feed()
  -> pacat --raw -> 私有 null sink -> dream -I <sink>.monitor -c 6 --sigsrate 48000
       |- --status-socket -> 按行 JSON -> state.sdr_drm
       `- -O <audio sink> -> parec -> 单声道 int16 -> AUDF 帧
```

音频通过第二个私有 null sink 用 `parec` 实时采集。Dream 自带的 `-w/--writewav` 不可用
（接收机在解码时该文件仍为 0 字节）。

## 如何选择

在 SDR 面板的解调器组中选择 `DRM`（该模式属于后端/服务端模式，浏览器没有对应内核）。选中期间，
`STATUS.sdr.drm` 携带解码出的元数据，DRM 读数显示台名、鲁棒模式、带宽、码率、编码与同步状态。
音频通过常规的 SDR 音频开关播放。

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

## 已知限制

合成 fixture `tests/fixtures/drm/drm_modeB_so3_48k.f32`（来自 `feature/drm-demod`）能正确解出**元数据**，
但 Dream 将其 MSC 帧报告为 `CRC_ERROR` 并拒绝其音频：这是 DecDRM 发射机与 Dream 之间的互操作差异，
而非构建问题。因此音频采集用真实录音验证。真实 DRM 广播无需改动即可解出音频。
