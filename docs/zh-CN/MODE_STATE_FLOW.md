# SWP / RTA 参数状态与模式流转

本文定义普通频谱（SWP）和实时频谱（RTA）的参数所有权、默认值、命令语义和前后端状态流转。

## 1. 参数所有权

SWP 与 RTA 的采集参数互相独立，任何 RTA 配置都不得写入 SWP 字段。

| 参数 | SWP 状态 | RTA 状态 | 是否共享 |
|---|---|---|---|
| Center / Span | `center_hz` / `span_hz` | `rta_center_hz` / `rta_span_hz` | 否 |
| Ref / Ref Mode | `ref_level` / `ref_mode` | `rta_ref_level` / `rta_ref_mode` | 否 |
| RBW | `rbw_mode` / `rbw_hz` | `rta_rbw_mode` / `rta_rbw_hz` | 否 |
| VBW | `vbw_mode` / `vbw_hz` | `rta_vbw_mode` / `rta_vbw_hz` | 否 |
| Sweep | `sweep_time_mode` / `sweep_time` | `rta_sweep_time_mode` / `rta_sweep_time` | 否 |
| SDK 实际值 | `actual` | `rta_actual` | 否 |
| Points / Window / Spur | SWP 专用 | 不适用 | 否 |
| Atten / Preamp / IF Gain | 设备 RF 前端 | 设备 RF 前端 | 是 |
| Reference Clock | 设备级 | 设备级 | 是 |

## 2. 应用启动默认值

这些值是 WebSA 启动时的确定性配置，不等同于设备 `SWP_ProfileDeInit` Preset。

### SWP

| 参数 | 默认值 |
|---|---:|
| Center | 1 GHz |
| Span | 100 MHz |
| Ref | 0 dBm，Manual |
| RBW | 100 kHz，Manual |
| VBW | 100 kHz，Manual |
| Points | 1000 requested，实际点数由 SDK 决定 |
| Window | Blackman-Nuttall (`1`) |
| Spur | Bypass |
| Sweep | minSWT (`0`) |
| Atten | Auto (`-1`) |
| Preamp | Auto |
| IF Gain | 2 |
| Gain Strategy | Low Noise |

### RTA

| 参数 | 默认值 |
|---|---:|
| Center | 1 GHz |
| Span | 50.78125 MHz（Decimate 1） |
| Ref | 0 dBm，Manual |
| RBW | Auto；实际值由 SDK/FFT size 返回 |
| VBW | Equal to RBW |
| Sweep | minSWT x4 (`2`) |
| Display Points | 1001 |

SAN-90 真机在默认 RTA span 下返回 RBW/VBW 约 30.153 kHz；不能用 `span/2000`
替代 SDK 实际值。12.6953125 MHz span 下真机实际 RBW/VBW 约 7.538 kHz。

## 3. 设备 Preset 默认

`SET_PRESET` 从 `SWP_ProfileDeInit` 缓存恢复 SWP 默认，并同时将 RTA 恢复到上表固定默认。
设备返回的 SWP 全扫宽可能包含保护带，WebSA 会按当前型号 `caps.fmin/fmax` 归一化，避免
请求范围超过 SAN-45/60/90 能力。

Preset 会恢复 SWP Center/Span、Ref、RBW/VBW 及模式、Points、Window、Spur、Sweep、
Atten、Preamp、IF Gain、Gain Strategy；Ref Mode 恢复 Manual。

## 4. STATUS 三层语义

- 顶层 `center/span/ref/rbw/vbw/sweep_*`：当前模式的设备有效值，供 UI 直接显示。
- `req.swp` / `req.rta`：两个模式各自持久保存的请求配置。
- `swp_actual` / `rta_actual`：SDK 最近一次成功配置返回的实际值。
- `actual`：当前模式 actual 的兼容别名。
- `config_version`：每次成功硬件重配置增加 1。
- `response_to`：命令响应 STATUS 中标识对应命令；周期 STATUS 不包含该字段。

前端只使用顶层有效值刷新当前模式，同时保留后端两套模式私有配置。

## 5. SWP 频率字段联动

### Center / Span

1. 用户首次点击输入框时自动全选当前内容；保持焦点后的继续点击可正常放置光标。
2. 用户编辑 Center 或 Span，整个 SWP 频率编辑区进入 dirty。
3. 周期 STATUS 仍更新全局状态，但不得覆盖 dirty 编辑框。
4. 单位按钮采用安全的双态语义：输入数字被编辑后，点击 `Hz/kHz/MHz/GHz` 按该单位立即提交；未编辑时只换算显示并保持物理值，不配置硬件。
5. 点击 Set、按 Enter 或点击已编辑字段的单位，一次发送：
   `SET_FREQ {center, span}`。
6. 后端保留用户输入的 center，并将 span 缩小到该 center 两侧可用的最大对称范围；只有 Full Span 会显式将 center 设为全频段中点。
7. SDK 成功后返回带 `response_to=SET_FREQ` 的 STATUS。
8. 前端清除 dirty，并使用 SDK actual 统一回填 Center/Span/Start/Stop。

### Start / Stop

1. Start 和 Stop 始终作为一组编辑，不对单字段失焦立即下发。
2. 点击 Set 或按 Enter，一次发送：`SET_FREQ {start, stop}`。
3. 后端拒绝 Stop <= Start 或小于 100 Hz 的范围。
4. 后端转换为唯一的 Center/Span，SDK 成功后统一回填四个字段。

禁止在同一命令混合 `center/span` 与 `start/stop`。

### Span Step

- `▼ / Full Span / ▲` 按钮行与频率输入框左边缘对齐。
- Auto Step 根据当前 SWP span 的约 1/10 选择最近的 1/2/5 档，例如 100 MHz→10 MHz、200 MHz→20 MHz。
- 用户编辑 Step 后转为自定义模式；Auto 按钮恢复随 span 联动。
- Step 保存为绝对 Hz，Span 单位变化时同步换算显示，不改变物理步进。
- 增减 span 均通过一次原子 `SET_FREQ {center, span}`，并受设备最小/最大 span 限制。

## 6. SWP -> RTA

1. 用户点击 RTA；按钮禁用并进入 pending，SWP UI 暂不乐观切换。
2. 前端只发送一条 `SET_MODE {mode:rta}`，不再重放 localStorage 中的 RBW/VBW/Sweep。
3. 后端停止旧会话，读取持久的 RTA 私有配置，执行一次 `RTA_Configuration`。
4. 成功后 `config_version +1`，响应 STATUS 顶层切换为 RTA actual。
5. 前端收到 mode=rta 后切换 UI、解除 pending，并显示 RTA Center/Span、实际 RBW/VBW。
6. SWP 的完整配置始终保留在 `req.swp`，期间不被 RTA 回读覆盖。

再次进入 RTA 时直接使用上一次后端 RTA 私有配置；服务重启或 Preset 后使用 RTA 默认值。

## 7. RTA 参数设置

- Center/Span：一次 `SET_RTA {center, span}`，span 映射到 `50.78125 MHz / 2^n`。
- RBW：`SET_RBW` 仅更新 RTA 私有 RBW；Auto 的显示值必须使用 SDK actual。
- VBW：`SET_VBW` 仅更新 RTA 私有 VBW；默认 Equal。
- Sweep：`SET_SWEEP` 仅更新 RTA 私有 sweep；默认 x4。
- Ref：`SET_REF` 仅更新 RTA 私有 Ref/Ref Mode。

每个命令最多执行一次 RTA 重配置并返回一个带 `response_to` 的 STATUS。

## 8. RTA 中的设备级参数

Reference Clock、Reference Clock Output、Atten、Preamp、IF Gain 和 Gain Strategy
是设备级共享参数。在 RTA 活跃时，这些命令必须写入 RTA Profile 并执行一次
`RTA_Configuration`，禁止调用 `SWP_Configuration`。

真机切换 Internal/External 及 Clock Output On/Off 时，命令响应约 105-145 ms，
约 360 ms 后恢复 RTAF；模式始终保持 RTA，不需要 SWP 往返恢复。无外部参考输入时
不得自动测试 `external_forced`。

## 9. RTA -> SWP

1. 用户点击 RTA 退出，只发送 `SET_MODE {mode:std}`。
2. 后端停止 RTA trigger，恢复一直保留的 SWP 私有配置并执行一次 `SWP_Configuration`。
3. 成功后 `config_version +1`；顶层 STATUS 切回 SWP actual。
4. RTA 配置继续保留在 `req.rta`，供下次进入使用。

## 10. Reference Level

### Manual

`SET_REF {mode:manual, ref}` 会实际配置当前模式的 SDK RefLevel，不再只改变前端 Y 轴。
前端显示 SDK actual；手动 Atten 导致 SDK 修正 Ref 时，请求值保留在 `req`，顶层显示实际值。

### Auto

1. `SET_REF {mode:auto}` 启用当前模式独立的 Auto Ref。
2. 只有峰值高于估计噪底至少 15 dB 时才识别为信号；无信号时保持当前 Ref。
3. Auto 下改变 Center 或跨模式返回时，若 Ref<0 会先临时回到 0 dBm，再以安全 Ref 调谐新频段。
4. 识别信号后以峰值上方约 5 dB 为目标，并量化到 5 dB 步进；目标低于 -50 dBm 时保持当前 Ref，噪底较高时保留约 30 dB 余量。
5. 任何 SWP/RTA 重配置都会清除旧候选，并暂停观测 0.75 秒。
6. 范围限制为 -50 到 +30 dBm（生效下限由上述规则决定）。
7. Ref 上调要求约 150 ms 连续稳定；Ref 下调要求目标连续稳定 1.5 秒。
8. 两次调整至少间隔 1 秒，避免重配置振荡。
9. 手动 Atten 时 Auto 保留但暂停；恢复 Atten Auto 后继续工作。
10. `auto_ref.last_peak/last_noise_floor/candidate/pending` 用于诊断。

改变 Ref 会清除/重建依赖显示幅度网格的 RTA density；SWP/RTA Auto 状态互不影响。

## 11. Marker Toggle 与 Tracking

- Marker 表第一列是每行独立 On/Off toggle；打开时按当前峰值排序选择未占用峰，关闭时保留位置和 Tracking 设置。
- Tracking 在 SWP POWR 与 RTAF 两条路径均生效，使用当前活动 Trace（含 Hold/Average）。
- 多个 Tracking Marker 首次按峰值强度依次占用不同峰；后续优先跟随记录频率附近的峰，避免跳到远端更强杂散。
- SWP/RTA 频率轴变化时先按 `marker.freq` 重定位，再执行 Tracking。

## 12. 真机验证基线

SAN-90 + TinySA Ultra+ ZS407，1 GHz / -25 dBm：

- SWP Auto Ref：峰值约 -26.8 dBm，Ref 从 0 dBm 收敛到 -20 dBm。
- RTA Auto Ref：峰值约 -29.67 dBm，Ref 从 0 dBm 收敛到 -20 dBm。
- SWP -> RTA：一次配置，默认返回 50.78125 MHz / Auto RBW / Equal VBW / x4。
- RTA -> SWP：一次配置，SWP Center/Span/RBW/VBW 完整恢复。
- Center/Span、Start/Stop、RTA Center/Span 每次提交均只增加一个 config version。
