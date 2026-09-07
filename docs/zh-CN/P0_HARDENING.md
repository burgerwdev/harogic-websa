# P0 稳定性与安全加固

本文记录 `feature/p0-hardening` 的实施范围、运行方式、真机验证和剩余边界。

## 已实施

- 默认监听从 `0.0.0.0` 改为 `127.0.0.1`。
- 远程监听必须配置 `WEBSA_TOKEN`，或显式设置不推荐的
  `WEBSA_ALLOW_UNAUTHENTICATED_REMOTE=1`。
- REST/WS 支持 Bearer/query token；WebSocket 校验同源或 `WEBSA_ALLOWED_ORIGINS`。
- 静态文件使用解析后根目录约束，修复编码绝对路径任意文件读取。
- 所有控制命令统一校验类型、有限浮点数、范围、枚举、设备连接和能力。
- `CONNECT` 在设备启动时未打开的情况下真正执行重连。
- 修复参考时钟校准路径未导入 `asyncio` 的运行时错误。
- SWP/RTA/GNSS/配置通过统一命令锁串行，阻塞 SDK 调用移出 asyncio 事件循环。
- 每个 WebSocket 客户端拥有独立有界发送通道；POWR/RTA 使用 latest-wins，
  FREQ 和 JSON 控制帧单独保留，慢客户端不阻塞采集和其他客户端。
- STATUS 增加 `stream.clients/dropped_frames/dropped_control` 运行指标。
- 非有限 SDK 浮点值在 JSON 中转为 `null`，避免浏览器 `JSON.parse` 失败。
- RTA 在硬件锁内固定元数据并复制 DLL 缓冲区，消除重配置 TOCTOU；退出先停止触发。
- 谐波全带宽刷新只配置一次，不再对同一参数连续配置 8 次。
- 前端支持 `wss`、令牌、退避重连、坏帧长度保护和坏 JSON 保护。
- RTA 重计算限制约 33 Hz，噪底/峰值分位数改为 O(n) 直方图估计。
- 前端频率范围由设备 STATUS 能力驱动，不再固定 SAN-90 范围。
- SWP/RTA 分别保存 Center/Span/Ref/RBW/VBW/Sweep/actual，模式切换不再互相覆盖。
- SWP Center/Span 与 Start/Stop 原子提交；编辑后单位按钮可直接确认并下发，未编辑时只安全换算显示；dirty 编辑不被周期 STATUS 覆盖。
- RTA 切换只发送一次 SET_MODE，不再由 localStorage 连发 RTA/RBW/VBW/Sweep 重配置。
- Reference Level Manual 实际下发硬件；Auto 采用峰值余量、时间稳定和迟滞控制。
- RTA 中切换 Ref Clock/Clock Output/共享增益时重配 RTA Profile，不再错误调用 SWP 配置。
- RTA 连续 8 次调用失败时自动原地重配，两次恢复失败后升级给 supervisor 重启 worker。
- Marker 表支持逐行 On/Off 和独立 Tracking，SWP/RTA 均按频率连续性追踪并支持多 Marker 分峰。
- 频率输入首次获得焦点时自动全选，继续编辑时不重复抢占选区。
- SWP 提供 `▼ / Full Span / ▲`，Step 默认跟随当前 span 的约 1/10 并归一化到 1/2/5 档；支持自定义和 Auto 恢复。
- supervisor 在 worker 原生崩溃或硬件超时后退避重启；配置错误不循环重启。
- NumPy/pyserial 依赖、DSP_Close、跨架构 SDK 路径和滚动日志已补齐。

## 本机运行

```bash
./build.sh
./run.sh
# http://127.0.0.1:8080
./stop.sh
```

远程监听：

```bash
WEBSA_HOST=0.0.0.0 WEBSA_TOKEN='replace-with-a-long-random-token' ./run.sh
```

浏览器使用：

```text
http://设备地址:8080/?token=replace-with-a-long-random-token
```

反向代理改变 Origin 时，通过逗号分隔配置：

```bash
WEBSA_ALLOWED_ORIGINS='https://sa.example.com' \
WEBSA_HOST=127.0.0.1 WEBSA_TOKEN='...' ./run.sh
```

## 自动化验证

```bash
./test.sh
python3 -m ruff check web_sa tests tools
cd frontend/modern && npm audit
```

当前基线：后端 50 项、前端 22 项，Ruff 0，npm audit 0。

## 硬件冒烟测试

服务启动后，只识别 TinySA、不修改其输出：

```bash
./tools/hardware_smoke.py --tinysa-port /dev/ttyACM0
```

显式配置 TinySA 并测试 1 GHz：

```bash
./tools/hardware_smoke.py \
  --tinysa-port /dev/ttyACM0 \
  --configure-tinysa \
  --tinysa-output-mode normal \
  --frequency 1e9 --span 10e6 --duration 3
```

工具的 TinySA 安全顺序：

1. `output off`，先设置全频段保守值 `level -42`。
2. `mode output` 并选择 `normal/mixer`，再次关闭输出和设置 `-42 dBm`。
3. 使用 `sweep cw <Hz>` 设置频率；`freq` 只设置测量频率，不能用于发生器。
4. 用 `sweep` 回读 start/stop，必须都等于目标频率。
5. 按频段强制设置 `<=6 GHz: -25 dBm`、`(6,7] GHz: -35 dBm`、
   `(7,9] GHz: -42 dBm`；高于 9 GHz 拒绝。
6. `output on` 后执行 `resume`，状态必须为 `Resumed` 才开始采集。
7. 无论成功失败，结束时执行 `output off`；SAN 恢复测试前 SWP 中心和扫宽。

## 真机结果

SAN-90 + TinySA Ultra+ ZS407，TinySA `1 GHz / -25 dBm / normal`：

- SWP：约 205.4 fps，166 个设备原生点，峰值约 999.985 MHz / -26.82 dBm。
- RTA：首帧后约 128.7 fps，1001 点，切换到首帧约 0.57 s，
  峰值约 999.996 MHz / -29.69 dBm。
- SWP/RTA 均无坏帧；静态逃逸请求返回 403。
- 模拟 worker `SIGKILL` 后，supervisor 约 3 秒重新打开 SAN 并恢复 API。

功率差不能直接作为仪器精度结论；仍需计入 TinySA 输出误差、线损、RBW/窗函数和
RTA 幅度量化，并与校准源或参考仪器对比。

## 剩余边界

- 当前 supervisor 能从进程崩溃/超时恢复，但 Web 服务和 SDK 仍在同一个 worker；
  后续 VSA 应按独立 Hardware Worker + IPC 架构彻底隔离。
- `os._exit` 是厂商 SDK 析构不稳定条件下的故意选择，退出不发送 WS close frame。
- 需要继续做 USB 热拔插、重复 SWP/RTA 切换、慢客户端、GNSS 校准和 6/7/9 GHz
  长时间压力测试。
- RTA 当前仍使用 PacketFrame 中第一条频谱，硬件 bitmap/PacketFrame 的真实概率密度
  语义需要与厂商文档和 SAStudio 对照。
- 远程访问建议由反向代理提供 TLS；query token 只能在 HTTPS 下使用。
