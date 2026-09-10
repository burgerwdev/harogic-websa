# 架构

## 总览
```
浏览器 (frontend/modern: TypeScript + Canvas)
   │  WS 有界 latest-wins 二进制帧 + WS JSON
   ▼
Supervisor → web_sa worker (aiohttp + 串行 SDK 调用；native 崩溃/超时退避重启)
   │
   ▼
htra_api.py → libhtraapi.so → USB → SAN 系列设备
```

## 后端分层
- `hardware/sdk_bindings.py`: 全部 ctypes 绑定(唯一 dll 接触点), 含 PNM 结构/校准函数
- `hardware/device.py`: 设备抽象(open/configure/fetch/query) + DeviceState + 型号能力推导
- `measurements/`: 测量会话对象化 (Std/RTA/Harmonic/PhaseNoise) + framer(帧编解码)
- `web/`: ws.py(命令表) + http_api.py(STATUS/REST) + publisher.py(按模式调度)

## 关键设计
1. **会话对象化**: std/rta/harmonic/pnm 统一接口, enter/exit 配置快照恢复
2. **型号能力推导**: DeviceCapabilities(SAN-45/60/90 频率范围), 不硬编码
3. **前端保峰重采样**: 后端返回设备原生迹线, 前端 resampleTrace 处理点数(保峰, 无插值三角)
4. **帧协议**: 16 字节头(magic+ver+points+sweep_ms) + 数据; POWR 强制 float32
5. **客户端背压**: 每个 WS 独立发送任务；FREQ/JSON 保留，POWR/RTAF 采用 latest-wins
6. **安全默认**: loopback 监听；远程模式要求 token；静态资源限制在构建目录内
7. **故障恢复**: SDK 调用离开 asyncio 主线程；native 崩溃/致命超时由 supervisor 重启 worker
8. **模式私有状态**: SWP/RTA 分别保存 Center/Span/Ref/RBW/VBW/Sweep 和 actual；
   模式切换只发送一次 SET_MODE，完整流转见 [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md)

## 帧协议
| 类型 | 头 | 数据 |
|---|---|---|
| FREQ | FREQ + ver(4) + points(4) + sweep_ms(f4) | float64 频率轴 |
| POWR | POWR + ... | float32 功率 dBm |

## WS 命令
CONNECT/STATUS/SET_PRESET/CAL_REFCLK/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_SWEEP/
SET_POINTS/SET_SPUR/SET_WINDOW/SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_RTA/SET_HARM/SET_PNM

## RTA 实时频谱 (SWP/RTA 模式)
- **会话** (`web_sa/measurements/rta.py`, `RtaSession`): 基于官方 SDK 路径
  `RTA_Configuration` → `BusTriggerStart` → `GetRealTimeSpectrum`; 官方取包模式
  (trigger 后循环 Get), `acq=0.005`; SAN-90 实测持续推送约 105-129 fps，常规模式切换首帧约 0.57 s
- **推送与显示**: 后端按设备 Get 速率采集；每客户端 latest-wins 防止慢连接阻塞；前端 RTA 数据处理
  节流约 33 Hz，SWP 绘制约 30 Hz
- **RTAF 帧** (`measurements/rta.py`): magic `RTAF` + `freq`(f8) + `spec`(f4) +
  `wfRow`(u2) + `stopHz`; 各 dtype 分别 `tobytes` 打包，当前迹线使用 PacketFrame 中第一条频谱
- **多迹线 (T1-T4)**: 各迹线独立累积(`rtaDisplays[4]`)并按各自 mode 叠加显示(各自颜色 + 辉光);
  **Freeze/View** 为 trace mode 下拉右侧的独立 toggle 按钮(VIEW = 冻结累积; 解冻恢复之前的模式);
  Clear 清空当前迹线
- **概率密度背景**: 默认 `freq × 128` 幅度 bin（可选 64/96/128/192），按信号迹线路径累积；
  衰减和颗粒度可配置，使用 O(n) 直方图估计噪底并在约 33 Hz 更新
- **瀑布**: SWP 从当前迹线生成并节流到约 10 行/s；RTA 从实时 spec 生成行；容器替换 Marker 表槽位，
  最新行位于顶部
- **恢复**: 连续 8 次 Trigger/Get 失败后按当前 RTA 私有参数原地重配，最多两次；仍失败时
  抛出致命硬件错误，由 supervisor 重启 worker；STATUS 暴露 `rta_health`
- **已知坑**: `renderRta` 用外层 `save/clip(plotRect)` 包裹密度+迹线, 画底部频率行前必须
  `restore` —— 否则绘图区外的频率行被 clip 裁掉, 切到 RTA 后消失(已修复)

## 前端 DSP 引擎 (marker 寻峰寻谷)
按现代频谱仪架构(Keysight/R&S 思路)实现, 位于 TypeScript DSP/渲染模块:
- **S-G 平滑** `sgSmooth(src,w,adaptive)`: 2 阶 Savitzky-Golay + 梯度自适应
  (|dY/df|>90% 分位 → 窗口缩到 3 保边沿); smoothBins>1 时启用(MAX/MIN/MEDIAN 保留原逻辑)
- **三级寻峰引擎** `findExtremesOrdered(dir,isPeak)`:
  1. 局部极值扫描(±1bin)
  2. Excursion 双侧追溯 ≥6dB(上限 100bin) 过滤噪纹
  3. 抛物线亚频点拟合 `parabolaFit`(Δk=-0.5(y2-y0)/denom → 亚频点频率/幅度)
- **Raw Anchor**(非平滑时): 平滑定位 → 原始迹线邻域取真实极值(深陷波不被拉浅)
- **去重**: 峰 3bin(窄峰保持); 谷 25bin(合并同一凹陷, 重建为凹陷内 disp 全局最低,
  谷列表项与 Valley 定位一致)
- **遍历语义**: 峰/谷统一频率方向(左=低频, 右=高频); pos 匹配取最近列表项(±3bin);
  不在列表(如 Valley 全局最低点非局部极小)时跳最近独立谷, 跳过同凹陷
- **Valley**: 定位显示数据全局最低点 + 抛物线亚频点
- **多 Marker Tracking**: Marker 表逐行 toggle；首次按峰值强度分配未占用峰，后续按 `marker.freq`
  连续跟随邻近峰；SWP/RTA 共用策略，频率轴变化时先重定位
- **smooth 数据源**: 开启时完全用平滑曲线(位置/幅度均平滑, 与显示一致); 关闭用原始+Raw Anchor
- **Pk 阈值**: 未设置自动=峰值-50dB; 用户编辑锁定(activeElement 不覆盖 + oninput 实时锁定);
  Auto 恢复; marker 全关闭不更新

## 显示与控制设计
- **Reference Level**: Manual 实际配置当前 SWP/RTA Profile；Auto 仅在峰值高于噪底至少 15 dB 时调整，
  Center/跨模式重调谐前将负 Ref 临时恢复到 0 dBm，并使用 5 dB 余量、稳定时间和迟滞；
  重配置后等待 0.75 秒，手动 Atten 时 Auto 暂停
- **周期 STATUS 推送(1s)**: publisher 每秒推送全量 STATUS(与 GNSS 轮询对齐),
  使 GNSS 锁定/时间、refclk_out、校准状态自动刷新, 无需刷新页面
- **GNSS 详情浮层**: 点击 GNSS 指示器查看完整信息(锁定/卫星/天线/经纬度/海拔/UTC 时间)


## 触发架构（RTA 设备 / SWP 软件）

- **RTA（设备）**: 触发参数写入 `RTA_Profile_TypeDef`（`TriggerSource/Edge/Mode`、门限、防抖、延迟、
  预触发、采集时长、重触发、触发输出），由 `web_sa/measurements/rta.py` 在每次配置时下发；取帧循环
  （`RTA_BusTriggerStart` + `RTA_GetRealTimeSpectrum`）在等待触发期间不产出数据包，命中后交付一帧。
  前端 `ui/trigger.ts` 负责面板、状态角标（`FREE/WAIT/TRIG`）、门限线；`ui/triggerEvents.ts` 在**帧路径**
  上判定"到达的包即捕获"（settle 窗口 500 ms，避免启用瞬间的在途帧被误判）。
- **SWP（前端软件）**: 设备在普通模式下没有电平触发。`dsp/levelCross.ts`（纯函数，含单测）比较相邻两次
  扫描的同频点电平得到穿越事件；`ui/swpTrigger.ts` 负责启用与逐帧判定；命中后帧路径停止更新画布以实现
  定格。判定与显示刻度无关，因此改 Ref 不影响触发。
- 界面共用一套按钮（`Capture` / `Free Run` / `Esc`），按当前模式分派到两套实现。

- **画布状态区是共享的**: 右上角的指示信息由 `render/statusStack.ts` 统一绘制——各功能在一次渲染中
  投递状态块（触发角标、限制线判定），按顺序向下堆叠、右对齐，因此新增指示不会与既有信息重叠。
- **限制线在两种模式都生效**: 判定核心（`dsp/limits.ts`）与显示无关，RTA 分支与扫频分支调用同一套
  `renderLimits()`；画布上给出 `LIMIT PASS` / `LIMIT FAIL n` + `最差 +x dB @ f`，与面板内的判定一致。


## 虚拟键盘（v1.3.4）

可选的屏幕数字键盘，默认关闭，开关状态记在 `localStorage['websa-keypad']`。`ui/keypad.ts` 只创建一次，
每次打开按字段填充：

- 带单位组的字段（`core/units.ts` 的 `UNIT_OPTIONS`）额外提供单位键；按单位键会写入数值并置
  `dataset.edited='1'`、调用面板自身的 `setUnit()`，因此与手点面板按钮发出的命令完全一致
- 标了 `data-keypad="text"` 的字段（n dB 门限列表）按纯文本编辑：显示行是真实输入框，鼠标可定位光标、
  退格删除光标处一个字符、`C` 清空整串；该字段的 `.` 键变为 `,` 分隔符
- 其余字段通过 `change` 事件提交；`ref`/`points`/`rbw`/`vbw`/`harm`/`pnm`/`ampdbs` 这类"暂存 + Set/Meas
  按钮"字段改为点击其自身按钮提交
