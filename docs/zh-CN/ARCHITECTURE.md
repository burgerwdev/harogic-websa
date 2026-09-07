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
- `measurements/`: 测量会话对象化 (Std/Harmonic/PhaseNoise) + framer(帧编解码)
- `web/`: ws.py(命令表) + http_api.py(STATUS/REST) + publisher.py(按模式调度)

## 关键设计
1. **会话对象化**: std/harmonic/pnm 统一接口, enter/exit 配置快照恢复
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
CONNECT/STATUS/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_POINTS/SET_SPUR/SET_WINDOW/
SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_HARM/SET_PNM

## RTA 实时频谱 (SWP/RTA 模式)
- **会话** (`web_sa/measurements/rta.py`, `RtaSession`): 基于官方 SDK 路径
  `RTA_Configuration` → `BusTriggerStart` → `GetRealTimeSpectrum`; 官方取包模式
  (trigger 后按 `PacketCount` 循环 `Get`), 切换延迟 ~1.5 s, `acq=0.005` (~150 fps 设备能力)
- **推送速率**: 后端 step 仅等待 `acq+0.002`(~7 ms) → ~130 fps; 前端渲染节流 16 ms → ~60 fps 显示
  (SWP 模式保持 33 ms)
- **RTAF 帧** (`framer/`): magic `RTAF` + `freq`(f8) + `spec`(f4) + `wfRow`(u2) + `stopHz`;
  各 dtype 分别 `tobytes` 打包(单次 `np.concatenate` 会提升为 f8 破坏前端解析); wfRow 在 density
  字节前(2 字节对齐); 丢弃首帧的 19 帧镜像
- **多迹线 (T1-T4)**: 各迹线独立累积(`rtaDisplays[4]`)并按各自 mode 叠加显示(各自颜色 + 辉光);
  **Freeze/View** 为 trace mode 下拉右侧的独立 toggle 按钮(VIEW = 冻结累积; 解冻恢复之前的模式);
  Clear 清空当前迹线
- **概率密度背景**: 2D 累积 `rtaDensity2d`(freq × 50 幅度 bin), 沿迹线路径画 1px 点
  (`fillRect` source-over, 网格保持可见); 信号门控(噪底 +15 dB), 峰 ±1 bin(中心 +3 / 两侧 +1.5);
  衰减 ×0.97, 新点 +3, 密度显示阈值 0.01(保留信号低功率底部, 避免空档), 最低亮度 90,
  dens/8 满亮; 离屏余辉已回退 → 荧光辉光迹线 + 密度点尾迹
- **瀑布**: SWP 模式 POWR 行节流 10 行/s; RTA 由后端推 `wfRow`; 容器替换 marker 表槽位
  (同高 135px → 频谱画布不跳动); top-down 增长(最新在顶部); RTA 瀑布用 spec 行
  (bitmap 行全 0 不可靠)
- **已知坑**: `renderRta` 用外层 `save/clip(plotRect)` 包裹密度+迹线, 画底部频率行前必须
  `restore` —— 否则绘图区外的频率行被 clip 裁掉, 切到 RTA 后消失(已修复)

## 前端 DSP 引擎 (marker 寻峰寻谷)
按现代频谱仪架构(Keysight/R&S 思路)实现, 全部在前端 app.js:
- **S-G 平滑** `sgSmooth(src,w,adaptive)`: 2 阶 Savitzky-Golay + 梯度自适应
  (|dY/df|>90% 分位 → 窗口缩到 3 保边沿); smoothBins>1 时启用(MAX/MIN/MEDIAN 保留原逻辑)
- **三级寻峰引擎** `findExtremesOrdered(dir,isPeak)`:
  1. 局部极值扫描(±1bin)
  2. Escursion 双侧追溯 ≥6dB(上限 100bin) 过滤噪纹
  3. 抛物线亚频点拟合 `parabolaFit`(Δk=-0.5(y2-y0)/denom → 亚频点频率/幅度)
- **Raw Anchor**(非平滑时): 平滑定位 → 原始迹线邻域取真实极值(深陷波不被拉浅)
- **去重**: 峰 3bin(窄峰保持); 谷 25bin(合并同一凹陷, 重建为凹陷内 disp 全局最低,
  谷列表项与 Valley 定位一致)
- **遍历语义**: 峰/谷统一频率方向(左=低频, 右=高频); pos 匹配取最近列表项(±3bin);
  不在列表(如 Valley 全局最低点非局部极小)时跳最近独立谷, 跳过同凹陷
- **Valley**: 定位显示数据全局最低点 + 抛物线亚频点
- **smooth 数据源**: 开启时完全用平滑曲线(位置/幅度均平滑, 与显示一致); 关闭用原始+Raw Anchor
- **Pk 阈值**: 未设置自动=峰值-50dB; 用户编辑锁定(activeElement 不覆盖 + oninput 实时锁定);
  Auto 恢复; marker 全关闭不更新

## 显示层设计
- **ref level 为纯显示参数**: 设备返回的功率已含衰减补偿(端口参考), 前端将 ref level 仅作为 Y 轴顶值(displayRef),
  绝不下发设备 — 避免设备 ref-atten 耦合(手动衰减强制 ref=atten-10)与宽带源下 Auto 衰减重配卡顿
- **周期 STATUS 推送(1s)**: publisher 每秒推送全量 STATUS(与 GNSS 轮询对齐),
  使 GNSS 锁定/时间、refclk_out、校准状态自动刷新, 无需刷新页面
- **GNSS 详情浮层**: 点击 GNSS 指示器查看完整信息(锁定/卫星/天线/经纬度/海拔/UTC 时间)
