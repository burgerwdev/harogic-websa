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

进入 SDR 会恢复用户自己的 SDR 设置（调谐、捕获带宽、解调模式、IF 带宽、去加重、音量、静噪、AGC）：
它们被持久化并在进入时重新应用，因为切换模式不是复位。扫频中心交接属于显式手势（Shift+点击 / 峰值
「听这里」，它**同时**设置捕获中心与解调频率），以及首次使用（尚无任何已存设置，此时还会按频段推导
解调/IF 带宽）。

**业界惯例。** 台式频谱仪的参考电平「Auto」在三大厂商都是**一次性动作**而非跟踪模式：Keysight 的
*Auto Scale*、R&S 的 *Auto Level*、Anritsu 的 *Auto Scale*。它们的说明书写明还会顺带调整别的量
（R&S 同时优化射频衰减，Keysight 可能连刻度一起改），但共享的放置规则一致，也是本项目实现的规则：
按用户面前的迹线算出**一个**参考电平（含峰值余量），让整条迹线落入刻度网格，并让噪声底落在下沿稍上方。
这类仪器上持续在线的是过载保护，不是自动量程；本项目采用同样的分工（见下规则 7-8）。

下面各项是本项目的常量（`hardware/auto_reference.py`）；不引用厂商数值，因为厂商并不公布相同的量。

1. `AUTO_SCALE`（或旧写法 `SET_REF {mode:auto}`）按最新迹线**执行一次**放置；不存在「持续跟踪」模式，`ref_mode` 恒为 `manual`。
2. 锚点是**噪声底、不是峰值**：目标把噪底放在下沿上方 `FLOOR_ANCHOR_DB`（ 8 dB，默认 10 dB/div 下约 1 格），只在峰值需要时才抬 Ref（余量 >= 10 dB，噪底高时约 30 dB）。量化到 5 dB Ref 网格后，噪底实际落在下沿上方 3-8 dB——这是 5 dB 步进能做到的「贴底」。
3. **不再有信号电平门限。** 以峰值为锚点才需要信号，以噪底为锚点不需要。旧的 `峰值 - 噪底 < 15 dB -> no_signal` 拒绝就是一个死区（见下表）：仅噪声的迹线比下沿低几 dB 时会被判为 `inside`，随后拒绝动作，于是 Auto 回答 `no_signal`、保持电平，迹线依旧有一部分落在画布外（实测：SWP 20 MHz / 1 MHz，Ref 0、默认 100 dB 窗口下噪底 -101 dBm）。`no_signal` 仍保留在 STATUS 取值表中——字段取值是契约——但放置逻辑不再产生它。
4. 放置已经合理时不应用任何变更：噪底在下沿上方 2-12 dB 且峰值余量 >= 8 dB。下界比 5 dB 网格的量化步进高 1 dB，所以比它更低的迹线确实不算可靠入窗，会被拟合；上界避免抖动的估计触发重配。
5. **设置变更会预备一次自动重拟合**（span、中心/起止、RBW、VBW、窗函数、SDR 抽取率——即 `geometry()` 里的任何量）。它在新几何的首个稳定帧执行，因为对旧几何合适的电平对新几何并不合适。只有该签名真的变化才会预备，首次决策后立即撤销，并由 `SAFETY_INTERVAL_S`（2 秒）限速，因此不可能退化成持续跟踪。某个模式的**首次** settle 不预备任何东西：连接或进入模式本身绝不会自行移动电平。
6. **用户手输的电平永不被反转**——无论逐帧逻辑还是设置变更重拟合（`user_level` 由手动 `SET_REF` 路径置位，被一次已应用的拟合清除）。要重新拟合它就按 Auto。dB/div 是纯显示设置：窗口高度随 Auto Scale 请求一起上报后端，因此改刻度后在下一次按下时才拟合（行为不变）。
7. **安全量程始终运行**，与 Atten 设置、是否按过 Auto 无关，但只做**保护方向**：IF 溢出（-12）每秒抬 5 dB；峰值高出上沿 10 dB 以上的严重削顶时抬一次，限速 2 秒。**绝不自动降低**——把噪声底推到下沿之外只是显示选择（`below_window`），量程不动它（要重新拟合请点 Auto）。
8. 拟合调低过 Ref 后，改变 Center 或跨模式返回会先抬回 0 dBm 再调谐（`prepare_retune`）；新几何随后按规则 5 获得自己的一次性重拟合。
9. 任何 SWP/RTA/SDR 重配置都会清除旧观测并暂停 0.75 秒。
10. `auto_ref.last_peak/last_noise_floor/target/result/seq/pending/adjusting` 用于诊断；`adjusting` 同时驱动按钮的
   忙碌指示，`result` 给出结果名（`applied`/`ok`/`no_signal`/`no_data`/`clipped`/`below_window`/`overflow`），`seq` 每次
   决策自增，使 UI 能把"新答复"与"上次决策的残留"区分开。重拟合发现放置已经合理时不报告新决策：
   没有任何电平移动，也就没有可宣布的事情。
11. SDR 走同一套拟合，但它拟合的是**显示**电平：该刻度由客户端负责并应用上报的 target，窗口可到 -160 dBm，
   且只有 IQS 电平差超过 3 dB 时才写器件。因此 SDR 的 target 按**显示范围**夹取（而不是器件的 -50..+30 dBm），
   写入器件的 IQS 电平则按器件范围夹取——与命令层对 `AUTO_SCALE.current_ref` 用的是同一条
   「按值的所有者校验」规则。

改变 Ref 会清除/重建依赖显示幅度网格的 RTA density；SWP/RTA Auto 状态互不影响。

### 旧规则错在哪里

上报的现象（「小 span、无外部信号：迹线不在画布中，或只有极小部分落在下沿以下」）是**分类**上的
死区，不是目标公式的问题：

| 峰值 - 噪底 | 噪底相对下沿 | 旧 `_decide` | 新 `_decide` |
|---|---|---|---|
| >= 15 dB | 在带内 | `ok` / 拟合 | 不变 |
| < 15 dB | 在带内 | `no_signal`，保持电平 | `ok`（无需变更） |
| < 15 dB | 下沿以下 0-3 dB | `inside` 后 `no_signal`，保持电平（**即上报问题**） | 拟合：`applied`，目标把噪底放到带内 3-8 dB |
| 任意 | 下沿以下 > 3 dB | 拟合（`below_window`） | 不变 |

该门限当初存在，是因为拟合一度以峰值为锚点。一旦锚点换成噪底，就永远有东西可放置，所以「没有信号」
只是一种普通放置情形。

### 三个上报设置的验证

无外部信号；默认 100 dB 窗口在 Ref 0 dBm 时下沿为 -100 dBm。观测值即采集路径交给控制环的那一对
（30 分位为噪底、帧最大值为峰值；无载波时峰值只比噪底高几 dB）：

| 模式 / 设置 | 观测噪底 | Ref 0 时的迹线 | 设置变更后 |
|---|---|---|---|
| SWP 中心 20 MHz / span 1 MHz | -101.3 dBm | 噪底在下沿以下 1.3 dB | 一次重拟合 -> Ref -5 dBm，噪底在窗口内 3.7 dB |
| RTA 中心 20 MHz / 带宽 1.59 MHz | -101.3 dBm | 同上 | 一次重拟合 -> Ref -5 dBm，全部入窗 |
| SDR 中心 20 MHz（窄捕获带宽） | -101.2 dBm | 同上 | 一次重拟合 -> Ref -5 dBm，全部入窗（显示刻度） |

证据：`tests/test_auto_reference.py::test_the_reported_scenarios_through_the_fake_backend` 以纯噪声迹线
驱动假设备及其会话走真实控制环（帧 -> 观测 -> 设置变更 -> 一次重拟合 -> 应用）；同一批场景在决策层
也有断言（`test_the_reported_no_signal_scenario_ends_inside_the_window`）。e2e 的假后端保留了合成载波，
因此 `state_regression` 9e 覆盖接线部分：设置变更不会撤销手动电平，随后按 Auto 能把整条迹线放进窗口。

## 11. Marker Toggle 与 Tracking

- Marker 表第一列是每行独立 On/Off toggle；打开时按当前峰值排序选择未占用峰，关闭时保留位置和 Tracking 设置。
- Tracking 在 SWP POWR 与 RTAF 两条路径均生效，使用当前活动 Trace（含 Hold/Average）。
- 多个 Tracking Marker 首次按峰值强度依次占用不同峰；后续优先跟随记录频率附近的峰，避免跳到远端更强杂散。
- SWP/RTA 频率轴变化时先按 `marker.freq` 重定位，再执行 Tracking。

## 12. 真机验证基线

SAN-90 + TinySA Ultra+ ZS407，1 GHz / -25 dBm：

- SWP Auto Scale：载波 -18.5 dBm、Ref 故意高 6 dB 时，一步落到拟合电平，耗时 0.11-0.12 秒（三次实测）；
  第二次点击报 `ok` 且 `config_version` 不变。
- RTA Auto Scale：同一套规则经 RTA profile 生效（`session._configure`）。
- SWP -> RTA：一次配置，默认返回 50.78125 MHz / Auto RBW / Equal VBW / x4。
- RTA -> SWP：一次配置，SWP Center/Span/RBW/VBW 完整恢复。
- Center/Span、Start/Stop、RTA Center/Span 每次提交均只增加一个 config version。


## 13. 触发状态（RTA 设备 / SWP 软件）

两套实现共用一套按钮，状态机各自独立：

| 模式 | 状态 | 画面 | 按钮 |
|---|---|---|---|
| RTA | free（自由运行） | 实时刷新 | `Capture` |
| RTA | waiting（设备等待门限穿越） | **清空**（设备此时不发送任何数据包） | `Stop`（高亮） |
| RTA | hit（已捕获） | 定格在捕获帧；角标 `TRIG hh:mm:ss` | `Capture again` |
| SWP | free | 实时刷新 | `Capture` |
| SWP | waiting（软件等待穿越） | **保持实时** | `Stop`（高亮） |
| SWP | hit | 定格；角标 `TRIG hh:mm:ss` + 命中频率/电平 | `Capture again` |

- 两者都可用 `Free Run` 或 `Esc` 解除；命中后不自动解除，便于查看捕获结果。
- 进入 RTA 会话时触发源重置为 `bus`（避免上次启用触发导致"进来没图"）；Auto Scale 不再影响触发（见 KNOWN_ISSUES 20）。
- 角标仅在触发就绪/命中时显示；自由运行时画布不显示任何触发文字。

## 14. 平均档位按模式独立

SWP 与 RTA 各自保存平均深度（`avgTarget` / `avgTargetRta`，档位 2/4/…/256/∞），切换模式互不覆盖。
有限 N 为指数平均（α = 2/(N+1)，永不冻结），`∞` 为累计均值。
