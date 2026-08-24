# API 文档

后端（`web_sa/`）对外接口：**HTTP REST** + **WebSocket**（JSON 命令/状态 + 二进制迹线帧）。

- 服务地址：`http://localhost:8080`（`/ws` 为 WebSocket）
- 前端：modern UI（`/`），静态资源 `/static/modern/dist/{file}`

---

## 1. HTTP 接口

### `GET /api/state`

返回完整设备状态（JSON，与 WS `STATUS` 同构）。

```bash
curl http://localhost:8080/api/state
```

响应示例：

```json
{
  "cmd": "STATUS",
  "connected": true,
  "device": "SAN-90",
  "device_detail": { "uid": "5230500C00380025", "model": 67, "hw": 8, "mfw": 14184, "ffw": 14184, "bus_speed": 12, "bus_ver": 1, "api_ver": "0.55.89", "warnings": "" },
  "center": 1000000000.0, "span": 600000000.0, "ref": 0.0,
  "rbw_mode": "auto", "rbw": 100000.0, "vbw_mode": "bypass", "vbw": 1000000.0,
  "points": 1000, "window": 1, "spur": "standard", "mode": "std", "pnm_supported": true,
  "caps": { "model": 67, "name": "SAN-90", "fmin": 9000, "fmax": 9000000000 },
  "preset_defaults": { "center": 1000000000, "span": 100000000, "rbw": 100000, "points": 1000, "ref": 0, "atten": -1 },
  "req": { "center": 1000000000, "span": 600000000, "points": 1000, "rbw_mode": "auto", "rbw": 100000, "vbw_mode": "bypass", "vbw": 1000000, "ref": 0, "spur": "standard" },
  "actual": { "center": 1000000000, "span": 600000000, "points": 985, "rbw": 100000, "vbw": 1000000 },
  "amp": { "atten": -1, "preamp": 0, "ifgain": 2, "gain_strategy": 0, "atten_actual": 30, "preamp_actual": 1, "ifgain_actual": 2 },
  "ref_clock": "internal", "has_docxo": false, "refclk_ppm": 0.0, "calibrating": false,
  "refclk_out": false, "last_cal_freq": 0.0, "gnss": { "lock": false, "sats": 0 },
  "last_error": ""
}
```

字段说明：

| 字段 | 类型 | 说明 |
|---|---|---|
| `connected` | bool | 设备是否已连接 |
| `device` / `device_detail` | str / obj | 设备名与详细信息（uid/model/hw/mfw/ffw/bus/api/warnings）|
| `center` / `span` / `ref` | number | 中心频率 / 扫宽 / 参考电平（Hz、dBm）|
| `rbw_mode` / `rbw` | str / number | RBW 模式（manual/auto）、分辨率带宽 |
| `vbw_mode` / `vbw` | str / number | VBW 模式（bypass/equal/tenth/manual）、视频带宽 |
| `points` | int | 请求点数（前端重采样目标）|
| `window` | int | FFT 窗：0=FlatTop 1=Blackman-Nuttall 2=LowSideLobe 3=Rectangle 4=Kaiser |
| `spur` | str | 杂散抑制（bypass/standard/enhanced）|
| `mode` | str | 当前测量模式（std/harmonic/pnm）|
| `caps` | obj | 型号能力（model/name/fmin/fmax）|
| `preset_defaults` | obj | 设备默认配置（Preset 用）|
| `req` / `actual` | obj | 请求值 / 设备实际值（点数/RBW 等设备原生值与请求可能不同）|
| `amp` | obj | 增益链配置：atten/preamp/ifgain/gain_strategy + 实际值 atten_actual/preamp_actual/ifgain_actual |
| `ref_clock` | str | 参考时钟源：internal/external/premium/external_forced |
| `has_docxo` | bool | 是否支持 DOCXO |
| `refclk_ppm` / `calibrating` / `last_cal_freq` | - | GNSS 校准状态 |
| `refclk_out` | bool | 参考时钟输出使能 |
| `gnss` | obj | GNSS 锁定状态（lock/sats）|
| `last_error` | str | 最近错误信息 |

### `POST /api/config`

通过 REST 发送命令（等价于 WS 命令，命令表见下）。响应为完整 STATUS。

```bash
curl -X POST http://localhost:8080/api/config \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "SET_FREQ", "center": 1090000000, "span": 600000000}'
```

### `GET /`

返回前端页面（modern UI 的 `dist/index.html`）。

### `GET /static/modern/dist/{file}`

前端静态资源（JS/CSS/字体）。

---

## 2. WebSocket 协议

### 连接

```
ws://localhost:8080/ws
```

连接建立后服务端立即开始推送迹线帧（FREQ/POWR）与状态。

### 2.1 客户端 → 服务端（命令）

JSON 对象：`{"cmd": "<COMMAND>", ...}`

| 命令 | 参数 | 说明 |
|---|---|---|
| `STATUS` | - | 请求一次完整状态（回发 STATUS）|
| `CONNECT` | - | 连接设备（如未连接）|
| `SET_PRESET` | - | 恢复设备默认配置（Preset）|
| `CAL_REFCLK` | `count?` | GNSS 1PPS 参考时钟校准（后台线程，期间校准状态 `calibrating=true`）|
| `SET_FREQ` | `center`, `span` | 设置中心频率/扫宽（span 自动收缩到设备范围）|
| `SET_REF` | `ref` | 设置参考电平（dBm）|
| `SET_RBW` | `mode?`（manual/auto）, `rbw?` | 设置分辨率带宽 |
| `SET_VBW` | `mode?`（manual/equal/tenth/bypass）, `vbw?` | 设置视频带宽 |
| `SET_POINTS` | `points`（51~4000）| 设置扫频点数 |
| `SET_SPUR` | `mode`（bypass/standard/enhanced）| 杂散抑制模式 |
| `SET_WINDOW` | `window`（0~4）| FFT 窗口 |
| `SET_AMP` | `atten`（-1~33）, `preamp`（0/1）, `ifgain`（0~3）, `gain_strategy`（0/1）| 增益链配置 |
| `SET_REFCK` | `mode`（internal/external/premium/external_forced）| 参考时钟源 |
| `SET_REFCKOUT` | `on`（bool）| 参考时钟输出使能 |
| `SET_MODE` | `mode`（std/harmonic/pnm）| 切换测量模式（会话）|
| `SET_HARM` | `f0`, `count`, `span` | 谐波测量参数（基频 Hz、次数、每谐波扫宽）|
| `SET_PNM` | `center`, `threshold`, `traceavg`, `start`, `stop` | 相噪测量参数 |

> 配置类命令（SET_*）执行后服务端自动回发最新 STATUS。

**命令示例：**

```js
// 设置频率并切换到谐波测量
ws.send(JSON.stringify({ cmd: 'SET_FREQ', center: 1e9, span: 100e6 }));
ws.send(JSON.stringify({ cmd: 'SET_MODE', mode: 'harmonic' }));
ws.send(JSON.stringify({ cmd: 'SET_HARM', f0: 1e9, count: 5, span: 1e6 }));
```

### 2.2 服务端 → 客户端

#### JSON 消息

| `cmd` | 触发 | 说明 |
|---|---|---|
| `STATUS` | 连接时 / 命令后 / 周期 | 完整状态（字段见 §1）|
| `HARM` | 谐波测量中 | 谐波结果：`{cmd:'HARM', list:[{n,f,amp,dBc,idx,inSpan}...]}` |
| `PNM` | 相噪测量中 | 相噪结果：`{cmd:'PNM', offset[], pn[], carrier_freq, carrier_power, progress, done}` |
| `ERROR` | 设备错误 | `{cmd:'ERROR', msg}` |

**PNM 消息字段：**

```json
{
  "cmd": "PNM",
  "carrier_freq": 999999900.0,   // 载波频率(Hz)
  "carrier_power": -20.1,        // 载波功率(dBm)
  "offset": [100, 1000, 10000, 100000, 1000000, 10000000],   // 频偏点(Hz)
  "pn": [-82.8, -95.5, -104.3, -117.1, -126.4, -132.0],      // 相噪(dBc/Hz)
  "progress": 25,                // 增量采集进度 0~100(首轮后持续刷新)
  "done": false                  // 首轮完整标志
}
```

> `pn` 中未更新段为 -500（无效）；`offset` 覆盖全范围且 `pn` 无无效点即测量图完整。

#### 二进制迹线帧

**16 字节头 + 数据：**

```
偏移  大小  类型    内容
0     4    bytes   magic: 'FREQ' | 'POWR'
4     4    u32     version（FREQ/POWR 配对序号）
8     4    u32     points（点数）
12    4    f32     sweep_ms（实测扫频时间）
16    ...         数据
```

| magic | 数据类型 | 说明 |
|---|---|---|
| `FREQ` | float64 × points | 频率轴（Hz），仅在版本变化时发送 |
| `POWR` | float32 × points | 功率迹线（dBm），与同版本 FREQ 配对 |

> **注意**：POWR 强制 float32（防插值升精度错位）；points 为设备原生点数（前端自行保峰重采样）。

---

## 3. 测量模式

### 谐波（Harmonic）

1. `SET_MODE {mode:'harmonic'}` → `SET_HARM {f0, count, span}`
2. 服务端逐谐波自动调谐（H1~H5…），每次结果推送 `HARM`

### 相噪（Phase Noise）

1. `SET_MODE {mode:'pnm'}` → `SET_PNM {center, threshold, traceavg, start, stop}`
2. 服务端增量采集（`PNM_GetPartialUpdatedFullTrace`），每帧推送 `PNM`（含 progress）

### 退出测量

`SET_MODE {mode:'std'}` → 恢复标准扫频。

---

## 4. Python 调用示例

```python
import asyncio, json, struct
import aiohttp

async def main():
    async with aiohttp.ClientSession() as s:
        # 1) REST 获取状态
        async with s.get('http://localhost:8080/api/state') as r:
            st = await r.json()
            print('connected:', st['connected'], '| mode:', st['mode'])

        # 2) WebSocket 接收迹线
        async with s.ws_connect('ws://localhost:8080/ws') as ws:
            await ws.send_str(json.dumps({'cmd': 'SET_FREQ', 'center': 1e9, 'span': 100e6}))
            async for msg in ws:
                if msg.type == aiohttp.WSMsgType.BINARY:
                    head = struct.unpack('<4sIIf', msg.data[:16])
                    magic, ver, pts, sweep_ms = head[0], head[1], head[2], head[3]
                    if magic == b'FREQ':
                        freq = struct.unpack_from('<%df' % pts, msg.data, 16)
                    elif magic == b'POWR':
                        # float32 功率
                        import array
                        pwr = array.array('f')
                        pwr.frombytes(msg.data[16:])
                        print('POWR', ver, 'points', pts, 'peak', max(pwr))
                elif msg.type == aiohttp.WSMsgType.TEXT:
                    obj = json.loads(msg.data)
                    print('JSON:', obj.get('cmd'))

asyncio.run(main())
```

---

## 5. 注意事项

- **单句柄**：设备同一时刻仅允许一个进程打开（SAStudio4 与 Web 服务互斥）
- **设备调用串行**：单进程 aiohttp，命令经 `_dispatch` 串行处理
- **bin 协议**：`libhtraapi.so` 为专有二进制，需从 HAROGIC 官方获取（`htra_api.py` 已随仓库附带）
- **SystemClockSource=External**：危险配置，勿用（会导致设备挂死）
