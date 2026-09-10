# HAROGIC FAQ 与运行说明

## SDK 调用约束

1. 同一设备同一时刻只允许一条 SDK 调用；WebSA 通过命令锁和设备锁串行执行。
2. 单台设备不支持 API 与 SAStudio4 同时运行；被占用时 `Device_Open` 会失败。
3. 配置函数可重复调用，未显式设置的字段来自 SDK Profile 默认值。
4. 连续采集需要循环调用 Get；浏览器断开时 WebSA 不持续读取频谱。

## SWP 与 RTA

- `SWP_GetFullSweep` 返回范围可能宽于请求范围，WebSA 使用 `DSP_InterceptSpectrum` 裁剪。
- 请求点数与设备实际点数可能不同，STATUS 分别提供 request 和 actual。
- RBW 由采样率、窗函数、抽取倍数和 FFT 点数共同决定；RTA Auto RBW 必须使用 SDK actual，不能简单使用 `span/2000`。
- SWP/RTA 分别保存 Center、Span、Ref、RBW、VBW、Sweep 和 actual。完整流转见 [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md)。
- RTA 连续 8 次 Trigger/Get 失败后原地重配置，最多两次；仍失败时 supervisor 重启 worker。
- `rta_health.error_streak/recovery_attempts` 可用于排查 RTA 停更。
- harmonic/PNM 测量进行中，SWP 专用命令（频率/Ref/RBW/VBW/Sweep/Points/Spur/Window/增益/参考时钟）会被拒绝并返回明确错误，避免打断测量会话；RTA 下 FFT 窗口/点数/杂散抑制同样只能用于 SWP。

## Reference Level

- Manual Ref 会实际配置当前 SWP/RTA Profile，不只是改变显示范围。
- Auto Ref 仅在峰值高于估计噪底至少 15 dB、且“峰值上方约 5 dB”的目标不低于 -50 dBm 时调整；噪底较高时保留约 30 dB 余量。无信号或峰值过弱时保持当前 Ref，不再向 -50 dBm 收敛。
- Auto 模式下改变 Center 或跨 SWP/RTA 返回时，若当前 Ref<0 会先临时回到 0 dBm，避免以过低 Ref 调谐到未知强信号。
- 任何 SWP/RTA 重配置后都会清除旧候选并等待 0.75 秒，再恢复 Auto Ref 观测。
- 手动 Atten 时 Auto Ref 保留但暂停；恢复 Atten Auto 后继续。
- 降低 Ref 和 RBW 通常可以降低显示噪底，但需留意输入过载。

## 参考时钟

- `ReferenceClockSource`: 0=Int，1=Ext（失锁自动回退），2=Int+ DOCXO，3=ExtForce。
- SAN-90 外部参考输入为 10 MHz；WebSA 设置 `ExternalSystemClockFrequency=10 MHz`。
- `EnableReferenceClockOut` 控制参考时钟输出，硬件支持能力取决于型号。
- RTA 中切换 Ref Clock/Clock Output 会重配 RTA Profile，不会调用 SWP 配置或退出 RTA。
- `SystemClockSource=External` 是危险配置，只能在厂商指导下使用；本项目不开放该设置。
- GNSS 1PPS 校准需要真实锁定的 1PPS 输入；无信号时厂商 DLL 可能阻塞。
- 没有有效外部参考时不要使用 `external_forced`。

## 相位噪声

- 100 Hz 和 10 MHz 边界频偏可能不准确；可用 SWP `span=2×最大频偏` 辅助检查。
- 典型最小输入功率为 -50 dBm。
- PNM 使用增量采集，每次 Get 返回部分更新结果。

## 配置环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `WEBSA_HOST` | `127.0.0.1` | HTTP/WS 监听地址 |
| `WEBSA_PORT` | `8080` | 服务端口 |
| `WEBSA_TOKEN` | 空 | REST Bearer token 与浏览器/WS query token |
| `WEBSA_ALLOWED_ORIGINS` | 空 | 额外允许的 Origin，多个值用逗号分隔 |
| `WEBSA_ALLOW_UNAUTHENTICATED_REMOTE` | 空 | 显式允许无 Token 远程监听，仅限可信隔离网络 |
| `WEBSA_LOG` | `INFO` | Python 日志级别 |
| `WEBSA_LOGFILE` | 空 | 可选滚动日志文件（5 MiB，3 个备份）|
| `WEBSA_STATIC` | 自动 | 前端目录覆盖路径 |
| `HTRA_API_LIB` | 按架构从 `/opt/htraapi` 推导 | `libhtraapi.so` 完整路径覆盖 |

远程监听必须配置 Token：

```bash
WEBSA_HOST=0.0.0.0 WEBSA_TOKEN='replace-with-a-long-random-token' ./run.sh
```

浏览器访问：

```text
http://设备地址:8080/?token=同一令牌
```

反向代理改变 Origin 时：

```bash
WEBSA_ALLOWED_ORIGINS='https://sa.example.com' \
WEBSA_HOST=127.0.0.1 WEBSA_TOKEN='...' ./run.sh
```

Query token 可能进入浏览器历史和代理日志，远程访问应使用 HTTPS。只有在可信隔离网络中，才可显式设置 `WEBSA_ALLOW_UNAUTHENTICATED_REMOTE=1`。

## 启动与故障恢复

始终从项目根目录使用脚本启动：

```bash
./build.sh
./run.sh
./stop.sh
```

`run.sh` 启动 supervisor 和 WebSA worker。native 崩溃、致命采集错误或 DLL 调用超时后，supervisor 会退避重启 worker；配置错误不会循环重启。SAN-90 上模拟 worker `SIGKILL` 后约 3 秒恢复 API。默认运行日志为 `/tmp/san90-web.log`。

如果设备仍无法恢复：

1. 确认 SAStudio4 和其他 API 进程已退出。
2. 等待数秒后执行 `./stop.sh && ./run.sh`。
3. 仍失败时重新插拔 USB；固件完全挂死时可能需要重启系统。

## 软件测试

完整门禁：

```bash
./test.sh
python3 -m ruff check web_sa tests tools
cd frontend/modern && npm audit
```

`./test.sh` 会运行后端 pytest、Ruff 和前端 Vitest，任一阶段失败都会返回非零状态。当前基线为后端 51 项、前端 23 项。

## SAN-90 / TinySA 硬件冒烟测试

服务启动后执行：

```bash
./tools/hardware_smoke.py --tinysa-port /dev/ttyACM0
```

该命令只读取 TinySA 身份，不修改 TinySA 输出；但会测试 SAN 的 SWP/RTA 并恢复 SAN 测试前的 SWP Center/Span。

显式控制 TinySA 发射 1 GHz：

```bash
./tools/hardware_smoke.py \
  --tinysa-port /dev/ttyACM0 \
  --configure-tinysa \
  --tinysa-output-mode normal \
  --frequency 1e9 --span 10e6 --duration 3
```

安全规则：

- `<=6 GHz`: `-25 dBm`
- `(6,7] GHz`: `-35 dBm`
- `(7,9] GHz`: `-42 dBm`
- `>9 GHz`: 拒绝执行

工具先关闭输出并设置 -42 dBm，再选择模式、用 `sweep cw` 设置并回读频率，最后设置对应安全功率并执行 `output on + resume`。无论成功失败都会执行 `output off`。工具不会恢复 TinySA 原频率/模式；共享台架使用前应记录原设置，结束后按需要恢复。

## 其他已知行为

- IF Gain 1/4 档在部分频点可能存在约 1 dB 幅度差。
- SAN-45/60/90 的标称范围分别为 9 kHz-4.5/6/9 GHz，功能相同但指标不同。
- `RefClkFreqOffset` 在部分固件上可能恒为 0，可使用校准后的计算值。
- RTA 当前使用 PacketFrame 中第一条频谱；硬件 bitmap/PacketFrame 的概率密度语义仍需与厂商软件对照验证。

## 迹线检波器

- `Auto Sample`（默认）：自动跟随信号类型，适合一般观察与 CW。
- `Sample` / `Pos Peak` / `Neg Peak` / `RMS`：固定检波方式；`Pos Peak` 峰值最贴顶、`RMS` 噪底更低。
- `Auto Peak`：面向脉冲/突发信号捕获（逐频点挑选峰值帧）。SAN-90 实测对稳定 CW 会丢失载波（峰值 −21.7 → −85 dBm、抖动 0.85 dB），因此**不在界面提供**，仅在脉冲场景下通过 API 使用。
- 检波器仅影响 SWP；RTA 与 harmonic/PNM 测量期间该命令会被拒绝。
- 比较检波器时需固定 span / RBW / 点数：窄 span + 大 RBW 时每频点合帧样本极多，各检波器会收敛到同一电平（SAN-90 实测 10 MHz span、1 MHz RBW 下五档均为 −21.6 dBm）；宽 span 下差异才显现（100 MHz span、RBW auto：PosPeak −21.6 / RMS −27.6 / Sample −36.6 / NegPeak −84.7 dBm）。因此读数差异属检波器语义与驻留条件，不是故障。


## 为什么普通模式的触发没有防抖/延迟/预触发？

这些是设备侧（RTA）的硬件时序功能：RTA 连续采集，可以按时间定义抖动窗口与预触发区间；
普通扫频每个频点只驻留微秒级，一条迹线自身没有时间轴，只剩"相邻两次扫描之间是否发生门限穿越"
这一个可判定的量。因此面板上这些项在普通模式下置灰，仅 RTA 可用。

## 为什么按了 Auto Ref 之后触发好像失效了？

这是当前已知问题：Auto Ref 会解除已启用的 RTA 触发（按钮回到 `Capture`、门限线消失），原因尚未定位。
**手动**调整 Ref 不受影响——即使门限被推到可视区之外，门限线也会钉在边缘显示（带 ▲/▼）。
启用触发后需要改参考电平时，请用手动 Ref，或改完后重新按 `Capture`。
