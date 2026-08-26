# Harogic SAN 系列频谱仪 Web 上位机 (SAN-45 / SAN-60 / SAN-90)

**English: [README.md](README.md)**

基于 **Harogic SAN 系列频谱仪**（SAN-45 / SAN-60 / SAN-90，海得逻捷）官方 SDK（`htra_api.py` + `libhtraapi.so`，USB 连接）的浏览器 Web 上位机。

![主界面 (深色)](screenshots/main_dark.png)

## 设备实拍

| SAN-90 | SAN-90 + WebSA 实拍 |
|---|---|
| ![SAN-90](screenshots/harogic_san-90.jpg) | ![WebSA 实拍](screenshots/harogic_san-90_websa.jpg) |

## 特性

- **频谱显示** — 清除写入 / 最大保持 / 最小保持 / 平均 / 查看冻结，4 条迹线，前端平滑
- **控制面板** — 中心/扫宽、RBW/VBW/点数、FFT 窗口（FlatTop / B-Nuttall / LowSideLobe / Rectangle / Kaiser，与官方一致）、衰减/前置放大/中频增益、参考时钟（内部/外部/外部强制 + 输出）
- **Marker 与 DSP 引擎** — 4 个游标 + **全部开启/关闭**(自动分布到各峰值)、寻峰寻谷遍历

  - Savitzky-Golay 平滑（2 阶 + 梯度自适应）
  - 三级寻峰引擎：局部极值 → Escursion 双侧 ≥6dB → 抛物线亚频点拟合
  - 谷凹陷合并（25bin，与 Valley 定位一致）、频率方向遍历
  - Raw Anchor（未平滑时取原始迹线真实极值）
- **实时频谱 (RTA)** — FPGA 引擎、多迹线(各 tab 独立模式)、概率密度背景(渐隐痕迹)、瀑布图
- **GNSS 详情浮层** — 点击指示器查看完整信息(锁定/卫星/天线/经纬度/UTC 时间); 状态每秒自动刷新
- **测量模式** — 幅度（n-dB 带宽）、谐波（H1~H5 服务器自动调谐）、相噪（6 档频偏 100Hz~10MHz）
- **归一化** — 直通校准、自适应吸收、显示层变换

## 界面截图

| 深色主题 | 中文界面 | 浅色主题 |
|---|---|---|
| ![主界面](screenshots/main_dark.png) | ![中文](screenshots/main_zh.png) | ![浅色](screenshots/main_light.png) |

| 多点寻峰 (MAX_HOLD + 平滑) | 谐波测量 | 相噪测量 |
|---|---|---|
| ![寻峰](screenshots/marker_peaks.png) | ![谐波](screenshots/measure_harmonic.png) | ![相噪](screenshots/measure_phasenoise.png) |
| 寻谷 (DUT 扫频, MAX_HOLD + smooth=5) | 幅度测量 (N dB) | — |
| ![寻谷](screenshots/marker_valley.png) | ![幅度](screenshots/measure_amplitude.png) | |
| 瀑布 + RTA 实时频谱 |
| ![RTA瀑布](screenshots/waterfall_rta.png) |

## 快速开始

### 前置条件

- Python ≥ 3.10（aiohttp）
- Node.js ≥ 18（仅重建前端时需要）
- Harogic SAN 系列频谱仪 + 官方 SDK：
  - `htra_api.py` — 已包含在仓库根目录（官方 Python 包装，HAROGIC 版权）
  - `libhtraapi.so` — **专有二进制，请从 HAROGIC 官方获取**；放入 `ctypes` 可搜索路径

### 1. 安装依赖

```bash
pip install -r requirements.txt          # aiohttp, pytest
cd frontend/modern && npm install && npm run build   # 前端构建(dist 已含)
```

### 2. 运行

```bash
./run.sh
```

浏览器打开 http://localhost:8080

### 3. 测试

```bash
./test.sh      # 后端 pytest + 前端 vitest
```

## 目录结构

```
harogic-websa/
├─ web_sa/               后端 (aiohttp 单进程, 设备调用串行)
│  ├─ hardware/          sdk_bindings.py(唯一 dll 接触点) / device.py
│  ├─ measurements/      Std/Harmonic/PhaseNoise 会话 + framer(帧协议)
│  └─ web/               ws.py / http_api.py / publisher.py
├─ frontend/
│  └─ modern/            TS 前端 (Vite + TypeScript, i18n + 主题)
│     └─ src/__tests__/  vitest 测试(DSP 引擎, 合成迹线)
├─ htra_api.py           官方 SDK Python 包装 (HAROGIC 版权)
├─ docs/                 文档 (en/ + zh-CN/): 架构 / API / 重构留痕 / 已知问题 / FAQ
├─ tests/                后端 pytest(协议/配置/设备状态/HTTP API)
├─ screenshots/          README 截图
├─ run.sh / stop.sh / clean.sh / build.sh / test.sh / Makefile
├─ pyproject.toml / requirements.txt / LICENSE / .gitignore
```

## 脚本

| 脚本 | 说明 |
|---|---|
| `./run.sh` | 启动服务（单次启动，不重试）|
| `./stop.sh` | 停止服务 |
| `./clean.sh` | 清理缓存/日志/构建产物 |
| `./test.sh` | 后端 pytest + 前端 vitest |
| `make run/stop/clean/build/test` | 同 Makefile 入口 |

## 测试

- **后端（14 项）**：帧协议编解码 + float32 类型守卫、配置常量与 SAN 型号能力推导、`DeviceState` 序列化与 `build_status` 载荷、HTTP API 端点（`/api/state`、`/api/config`，aiohttp TestClient + stub 设备）——**无需硬件**
- **前端（13 项）**：DSP 引擎合成迹线测试——S-G 平滑（保峰/保边沿）、抛物线亚频点拟合、Escursion 过滤、寻峰寻谷与 25bin 凹陷合并、保峰重采样、归一化参考构建——**无需硬件**

## 开源说明

- 项目代码（后端 + 前端）：**MIT** 协议
- `htra_api.py`：HAROGIC 官方 SDK Python 包装，版权归 HAROGIC（海得逻捷）；随项目附带供配合自有 SDK 使用
- `libhtraapi.so`：专有二进制，**不在本仓库**，需从 HAROGIC 官方获取
- 截图使用真实 SAN-90 + TinySA 扫频源拍摄


---

## 作者

[好奇牛马 (B站)](https://space.bilibili.com/28447213)
