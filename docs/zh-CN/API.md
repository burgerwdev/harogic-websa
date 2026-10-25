# API 文档

原生后端（`backend/`）通过 **HTTP REST** 和 **WebSocket** 提供接口。WebSocket 传输 JSON 命令、状态和二进制迹线帧。仓库根目录的 `./run.sh` 默认连接硬件，服务端口为 8080；Python 客户端仅为可选工具。

- 服务地址：`http://127.0.0.1:8080`（WebSocket 路径为 `/ws`）
- 前端页面：`/`，静态资源：`/static/dist/{file}`
- 默认仅监听本机回环地址；设置 `WEBSA_HOST=0.0.0.0` 可供局域网访问，产品接口无需认证。
  WebSocket 仅允许同源请求或 `WEBSA_ALLOWED_ORIGINS` 白名单中的来源。

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
  "caps": { "model": 67, "name": "SAN-90", "fmin": 9000, "fmax": 9000000000,
            "ref_min": -50, "ref_max": 30, "rta_span_max": 50781250, "rta_points": 3328 },
  "preset_defaults": { "center": 1000000000, "span": 100000000, "rbw": 100000, "points": 1000, "ref": 0, "atten": -1 },
  "req": { "center": 1000000000, "span": 600000000, "points": 1000, "rbw_mode": "auto", "rbw": 100000, "vbw_mode": "bypass", "vbw": 1000000, "ref": 0, "spur": "standard" },
  "actual": { "center": 1000000000, "span": 600000000, "points": 985, "rbw": 100000, "vbw": 1000000 },
  "amp": { "atten": -1, "preamp": 0, "ifgain": 2, "gain_strategy": 0, "atten_actual": 30, "preamp_actual": 1, "ifgain_actual": 2 },
  "ref_clock": "internal", "has_docxo": false, "refclk_ppm": 0.0, "calibrating": false,
  "refclk_out": false, "last_cal_freq": 0.0, "gnss": { "lock": false, "sats": 0, "docxo": 0, "docxo_mode": -1, "antenna": -1, "latitude": 0.0, "longitude": 0.0, "altitude": 0, "time": "0000-00-00 00:00:00" },
  "last_error": ""
}
```

字段说明：

| 字段 | 类型 | 说明 |
|---|---|---|
| `connected` | bool | 设备是否已连接 |
| `device` / `device_detail` | str / obj | 设备名与详细信息（uid/model/hw/mfw/ffw/bus/api/warnings）|
| `center` / `span` / `ref` | number | 当前模式实际生效的设备中心频率 / 跨度 / 参考电平 |
| `ref_mode` | str | 恒为 `manual`：Auto Scale 是一次性动作，不是跟踪模式 |
| `rbw_mode` / `rbw` | str / number | 当前模式 RBW 模式及 SDK 实际值 |
| `vbw_mode` / `vbw` | str / number | 当前模式 VBW 模式及 SDK 实际值 |
| `points` | int | SWP 请求迹线点数；实际返回的原生点数见各 FREQ/POWR 帧 |
| `window` | int | FFT 窗：0=FlatTop 1=Blackman-Nuttall 2=LowSideLobe 3=Rectangle 4=Kaiser |
| `spur` | str | 杂散抑制（bypass/standard/enhanced）|
| `detector` | str | 迹线检波器（auto/sample/pos_peak/neg_peak/rms/auto_peak），仅 SWP |
| `frame_detector` | str | Frame Avg 请求；原生 Preset 恢复 `device_default`，由 SDK 默认 profile 决定帧间检波器 |
| `mode` | str | 当前测量模式（std/harmonic/pnm/rta/sdr）|
| `caps` | obj | 型号能力：`model`/`name`/`fmin`/`fmax`，以及客户端应读取而非硬编码的数值限制：`ref_min`/`ref_max`（dBm，用于自动调整参考电平，不限制手动输入范围，见 FAQ_NOTES）、`rta_span_max`（Hz）、`rta_points` |
| `preset_defaults` | obj | 设备默认配置（Preset 用）|
| `req` / `actual` | obj | 当前模式请求/实际值；`req.swp`、`req.rta`、`req.sdr` 分别保存各模式配置 |
| `swp_actual` / `rta_actual` | obj | SWP/RTA 最近一次 SDK 实际配置，互不覆盖 |
| `sweep_ms` / `sweep_ms_meas` | number | SDK 估算的 SWP 最短扫频时间（ms，用于 FREQ/POWR 帧头；无估算时为 0）/ 发布器实测单步采集/发布耗时 EMA（ms，仅 STATUS）。SWT 优先显示设备估算；为 0 时固定显示带“实测”标记的 EMA，不留空也不混写两个数据源。|
| `sdr` | obj | 请求的调谐/解调/音频设置、`actual` 捕获/显示/DDC 几何、`level_dbfs`、静噪、厂商 ADM 指标与原生流健康状态。`sdr.level_dbfs` 为频道 1 电平；每个 `sdr.channels[].level_dbfs` 测量本频道 AGC/音频增益前归一化复数 IQ，公式为 `10*log10(mean(I²+Q²))`，wire 满量程为 1。无有限测量（包括零功率）、频道停用、流身份/调谐改变、退出 SDR 模式或输入超过 1500 ms 时为 null。每个频道的 `channel_dbm` 为新鲜的 SDK 调谐点频谱峰值；`squelch_open` 为实际处理后的模拟音频门状态，在尚未处理、流已过期/停用或绕过该门的 DRM 编码音频模式下为 null。顶层 `sdr.channel_dbm` 和 `sdr.squelch_open` 回显频道 1。请求显示带宽可能窄于设备捕获带宽。|
| `sdr.filter_limits` | obj | 当前模式的动态 `ifbw` 几何范围：`mode`、`min_hz`、`max_hz`。`min_hz` 为真实 DSP 下限——AM/DSB/CW/NFM/WFM/PM 为 50，USB/LSB 为 101；协议锁定或未知模式返回 `min_hz:0, max_hz:null`。`max_hz` 是按当前采集中心/带宽、输入采样率、PFB tap、收听频率与 CW pitch 求得的整数采样上限；非 SDR、频道 1 停用或无可行范围时为 null。它不是模式推荐值，也不包含其他已启用频道的成本准入，范围内的值仍可能被拒绝。 |
| `sdr.receiver_error` | string 或 null | 共享 receiver worker 的故障诊断；无接收器故障时为 null。故障停止全部音频/解码接收器，采集、原始 IQ 和显示流继续运行；与全局 `last_error`、`ddc_error` 分开。 |
| `rta_actual.frame_points` | int | RTA 设备 FFT 帧宽；RTA2 传送返回频点，不受 SWP 请求点数限制 |
| `config_version` | int | 每次成功硬件重配置递增 |
| `response_to` / `request_id` | str / scalar | 命令 STATUS/ERROR 的命令名及可选标量请求标识；周期发布不携带请求身份 |
| `auto_ref` | obj | 有界 Auto 会话状态：adjusting；pending 为等待新数据确认的硬件或显示目标；hardware_adjusting 区分 SDR 采集恢复/音频过渡，仅显示拟合时为 false；另有 seq、result、last_peak、last_noise_floor 和成功结束时的 target（其余为 null），详见下文 |
| `rta_health` | obj | 当前 RTA 连续错误数和原地恢复尝试次数 |
| `amp` | obj | 共享请求 atten/preamp/ifgain/gain_strategy；atten_actual/preamp_actual/ifgain_actual 来自当前模式 SDK 实际配置，缺失为 null，不能用请求值补齐。前放 0 为配置 Auto、1 为 Off，不代表物理开关状态 |
| `ref_clock` | str | 参考时钟源：internal/external/premium/external_forced |
| `has_docxo` / `has_premium_ref_clock` | bool | 是否安装 DOCXO 硬件 / 是否有可用的高品质内部参考时钟；取决于硬件选件，与 GNSS 锁定状态和工作模式无关 |
| `refclk_ppm` / `calibrating` / `last_cal_freq` | - | GNSS 校准状态 |
| `refclk_out` | bool | 参考时钟输出使能 |
| `gnss` | obj | GNSS 状态：`lock`(0/1) `sats`(卫星数) `docxo`(0/1) `docxo_mode`(0=驯服,1=保持) `antenna`(0=外部,1=内部) `latitude`/`longitude`(度) `altitude`(米) `time`(UTC, 无效为 "0000-00-00 00:00:00")；`stale`标记未取得可用新快照 |
| `last_error` | str | 最近的全局错误；receiver worker 故障使用 `sdr.receiver_error` |

完整参数所有权、默认值和模式转换流程见
[`MODE_STATE_FLOW.md`](MODE_STATE_FLOW.md)。

### `POST /api/config`

通过 REST 发送命令（等价于 WS 命令，命令表见下）。响应为完整 STATUS。

```bash
curl -X POST http://localhost:8080/api/config \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "SET_FREQ", "center": 1090000000, "span": 600000000}'
```

### `GET /`

返回前端页面（`dist/index.html`）。

### `GET /static/dist/{file}`

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
| `SET_FREQ` | `center`,`span` 或 `start`,`stop` | 原子设置 SWP 频率窗口；禁止混合两种赋值 |
| `SET_REF` | `mode`（manual/auto）、`ref?`、`range_db?` | 当前模式参考电平；manual 必须提供 -160..40 dBm 的 ref；mode=auto 执行一次 Auto Scale |
| `AUTO_SCALE` | `range_db?`（5..200）、`current_ref?`、`display_only?` | 有界参考电平放置与过载恢复，详见下文；current_ref 不含外部 Offset，浏览器从显示 Ref 中减去补偿后发送 |
| `SET_RBW` | `mode?`（manual/auto/one_thousandth_span/one_percent_span）、`rbw?` | SWP 分辨率带宽；RTA 仅 manual/auto |
| `SET_VBW` | `mode?`（manual/equal/tenth/onethousandth/bypass）、`vbw?` | 设置视频带宽 |
| `SET_SWEEP` | `mode?`（0..8）、`time?`（schema：0..1000） | 0=minSWT、1=x2、2=x4、3=x10、4=x20、5=x50、6=xN、7=Manual、8=minSMPxN；time 在 Manual 为秒，在 xN 为倍率，仍须通过对应 handler 的校验。RTA 更新自身 Profile |
| `SET_POINTS` | `points`（整数 51..16000） | SWP 请求点数；RTA 点数由 SDK 决定 |
| `SET_SPUR` | `mode`（bypass/standard/enhanced）| 杂散抑制模式 |
| `SET_WINDOW` | `window`（0~4）| FFT 窗口 |
| `SET_DETECTOR` | `mode`（auto/sample/pos_peak/neg_peak/rms/auto_peak）| 迹线检波器，仅 SWP 生效 |
| `SET_AMP` | `atten?`（-1~33）, `preamp?`（0/1）, `ifgain?`（0~3）, `gain_strategy?`（0/1）, `request_id?` | 只更新提供的增益字段；STATUS/ERROR 回传 request_id，请求值与 SDK 实际回读分别保留 |
| `SET_REFCK` | `mode`（internal/external/premium/external_forced）| 请求参考源并重配当前模式；无高品质内参考硬件时在变更前拒绝。`actual.refclk_src`报告SDK实际配置源，包括外参考自动回退 |
| `SET_REFCKOUT` | `on`（bool） | 参考时钟输出，重配置当前模式 Profile |
| `SET_MODE` | `mode`（std/harmonic/pnm/rta/sdr）| 切换测量模式（会话）|
| `SET_RTA` | `center?`, `span?` | 原子设置 RTA 中心和 2^n 档分析带宽 |
| `SET_SDR` | `center?`, `decimate?`（2 的幂，1~`caps.decimate_max`；自动取整到最近 2 的幂）, `pfb_taps_per_phase?`（原生后端 4/8/16，默认 16）| 设置 SDR 捕获窗口；原生后端按每相 PFB 抽头数重建信道化器。16 tap 为默认全抑制档，8/4 为实测较低邻道/混叠抑制档 |
| `SET_SDR_TUNE` | `listen`（Hz）| 调谐主接收器。原生后端在采集范围内更新频道计划；已启用主路调到采集范围外时，将共享采集中心移到请求频率。保留全部三路频道配置；已启用辅路必须能容纳于新采集范围，辅路调谐不会移动采集中心。设备频率限制、占用带宽、粗箱通带及准入错误均显式报告 |
| `SET_SDR_DEMOD` | `mode?`, `deemph_us?`（-1=自动 0=关 ..1000）, `ifbw?`（正有限值，Hz）, `squelch?`（-200~0，SDK 标称 dBm 调谐点频谱峰值，AGC 前；<=-160 为关闭静噪）, `volume?`（0~2）, `pitch?`（CW 为 100..1600 Hz；其他模式为 (0,20000] Hz）, `agc?`（bool）, `am_sync?`（bool，默认 false）| 主接收器设置。原生后端处理全部 12 模式：am/dsb/usb/lsb/cw/nfm/wfm/pm/ft8/sstv/drm/drmplus，输出 AUD2 PCM 和 SDE2 事件。浏览器控制播放并显示结果。`ifbw` 请求修改接收滤波器，通过准入后重配信道化器 |
| `SET_SDR_CHANNEL` | `channel?`（1–3，默认 1）、`enabled?`、`freq?`/`listen?`、`mode?`/`demod?`、`ifbw?`/`if_bw?`、`squelch?`、`volume?`、`agc?`、`pitch?`、`deemph_us?`/`deemph?`、`nr?`、`nr_strength?`、`nr_algorithm?`、`nr_atten_db?`、`stereo?`、`am_sync?`、`audio_rate?`、`sstv_mode?` | 局部更新一个接收器；省略字段保留原值，冲突别名被拒绝。`am_sync` 仅接受 JSON 布尔值，默认 false。静噪范围 -200..0，CW pitch 为 100..1600 Hz，其他模式为 (0,20000] Hz，NR 强度 0..1，算法 wiener/dfn，衰减 0..100 dB，音频率为整数 8000..192000；SSTV 模式为 null（自动）或支持的模式索引。音量/去加重范围同上。 |
| `SET_HARM` | `f0`, `count`, `span` | 谐波测量参数（基频 Hz、次数、每谐波扫宽）|
| `SET_PNM` | `center`, `threshold`, `traceavg`, `start`, `stop` | 相噪测量参数 |
| `SET_TRIGGER` | `source`（bus/freerun/level/external/timer）, `edge`（rising/falling/double）, `level`（dBm，以 `caps` 界限为准）, `safetime`/`delay`/`pretime`（0~10 s）, `acqtime`（0.0005~60 s）, `retrigger`（0~65535）, `retriggerperiod`（s）, `out`（none/per_hop/per_sweep/per_profile）, `outpolarity`（positive/negative）| RTA 采集触发（写入 RTA Profile）：在 RTA 中立即重配，否则在下次进入 RTA 生效。SWP 的电平触发由前端软件实现，不经过该命令 |

静噪滑块范围为 -160..0，步进 0.1 dB，最小值关闭静噪。提高门限要求更强的信号才开启音频。
信号达到或超过门限时开门，低于门限 3 dB 且 300 ms 保持时间已过时关门，音量平滑渐入/渐出。
射频测量缺失或过期时开门放行。射频参考为 SDK 频谱中调谐频率附近一个频点间距内的峰值，
并非积分频道功率或 IQ dBFS 电平，绝对射频校准精度尚未独立验证。协议解码在此听音门之前进行。

原生 FM 音频在 `agc=false` 时使用固定 PCM 增益。应用音量增益前，NFM 的 5 kHz 频偏和
WFM 的 75 kHz 频偏对应满幅。WFM 波形事件使用独立的 20 Hz 样本时钟。

AM 默认使用包络检波，`am_sync:true` 切换到载波恢复同步检波；非 AM 模式仍保留该偏好。仅修改 `am_sync` 时，两种检波先对齐极性再平滑过渡，不重配采集、滤波器或重采样器，不改变流身份，也不重置输入和音频时钟。STATUS 的 `sdr.channels[].am_sync` 返回已确认值，主路同时映射到 `sdr.am_sync` 和 `req.sdr.am_sync`。

`sdr.channels[].am_sync_generation` 是服务端维护的只读 u32，默认 0；主路同时映射到 `sdr.am_sync_generation` 和 `req.sdr.am_sync_generation`。仅在 `am_sync` 改变时递增，重复设置相同值不改变代次，请求不能指定该字段。Preset 随新流身份将其复位。

原生 WFM 默认通过 AUD2 输出左右声道交错立体声，显式 `stereo:false` 输出单声道。DRM/DRM+ 的编解码音频不使用模拟 AGC、NR、静噪和去加重，界面禁用这些控件，保留音量和播放。停用频道可在未设置有效频率时局部更新，启用时再校验调谐。SSTV 的 `sstv_mode:null` 为自动识别，支持的整数值为固定模式；浏览器确认进入 SDR 后才重放保存选择。显示抽取、`sdr.actual.iq_rate` 捕获采样率与频道 `actual_iq_rate` 分别表示不同量。

命令先校验完整的候选频道配置，再修改状态。CW pitch 超出 100..1600 Hz 时返回 `bad_parameter`，不触发接收器故障。切换 NR 保留解码历史和连续的 `audio_index`；源位置跳过被丢弃的降噪器尾部。DRM/DRM+ 保留 NR 偏好，但实际 NR 为 Off。DFN 初始化失败或运行条件不受支持时选择 Wiener；运行时错误触发接收器故障，不回退。

`ifbw` 的含义随模式变化。AM/DSB/NFM/WFM/PM 中，它表示以拨盘频率为中心的总带宽。USB/LSB 中，它表示外侧高截止，内侧载波保护带为 `clamp(0.1 × ifbw, 100, 300)` Hz，LSB 取镜像负频率。CW 以 `max(100, pitch)` Hz 为中心，两侧各延伸 `max(50, ifbw) / 2`。浏览器数值编辑器与后端均对其他可编辑模式执行 50 Hz 下限，对 USB/LSB 执行 101 Hz 下限，避免保护带退化。浏览器还检查当前 `sdr.filter_limits`；后端另按所选模式的包络、采集和输出采样率、粗箱通带及频道容量执行准入。非法或不能准入的请求会被拒绝，不会静默截断。预设是推荐值而非上限：AM 100 kHz、SSB 50 kHz、CW 10 kHz 等高于常用预设的自定义值在通过准入时允许使用。输入支持 Hz/kHz/MHz，单位切换不改变底层 Hz 值，虚拟键盘与手动输入共用校验和提交路径。

浏览器将 FT8 固定为 `ifbw=3000`，解码范围为 100~3000 Hz；SSTV 固定为 `ifbw=3000`，解码范围为 300~2700 Hz；为 DRM 选择并锁定 `ifbw=12000`；DRM+ 固定为 `ifbw=100000`。这些是界面的模式语义，不是通信协议限制：直接 API 请求仍可使用其他值，但受后端几何与准入约束。DRM 显示 `±confirmed_ifbw/2`，不随 FAC 声明的占用带宽改变，因此不会自动接收全部 4.5~20 kHz 占用带宽。DRM+ 的 100 kHz 界面值是频道宽度，不表示最外侧 OFDM 载波恰好位于 ±50 kHz。

频道准入使用调谐点两侧的对称占用带宽包络。USB/LSB 覆盖单边 IF 上限；CW 包含 pitch 和 IF；FT8 至少保留正频率 3.2 kHz，SSTV 保留正频率 3 kHz。整数 IQ 采样率由该包络决定，精确协议采样率在内部适配。WFM IQ 还需支持导频、立体声和 RDS，与 PCM 播放采样率相互独立。主路重新采集前会验证全部已启用频道；请求被拒绝时，旧计划继续生效。

> 命令响应 STATUS 和 ERROR 均携带 response_to。可选 request_id 为字符串、数字、布尔值或 null 时原样回传，对象和数组不回传。周期状态和采集错误不携带请求身份。SWP/RTA 重配置被拒绝时，在恢复成功的前提下保留原请求和已应用 Profile。非法 REST 命令返回 HTTP 400，WS 返回带 msg 的 ERROR。
>
> 周期 STATUS 的 `stream` 字段包含 `clients`、`dropped_frames`、`dropped_control`、
> `dropped_audio`、`dropped_iq`，用于观察慢客户端触发的 latest-wins 丢帧与流式 FIFO 溢出。

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
| `STATUS` | 连接时 / 命令后 / **周期推送（每 1 s）** | 完整状态（字段见 §1）；周期推送更新 GNSS、时间、锁定等状态，无需刷新页面 |
| `HARM` | 谐波测量中 | 谐波结果：`{cmd:'HARM', list:[{n,f,amp,dBc,idx,inSpan}...]}` |
| `PNM` | 相噪测量中 | 相噪结果：`{cmd:'PNM', offset[], pn[], carrier_freq, carrier_power, progress, done}` |
| `ERROR` | 设备/命令错误 | `{cmd:'ERROR', code, params?, msg, response_to?, request_id?}`；稳定错误码供前端本地化，未知码回退英文 msg |
| `RECEIVER_ERROR` | 共享 receiver worker 故障 | `{cmd:'RECEIVER_ERROR', error:string, failed:true, scope:'all_receivers'}`；保留接收器诊断，不在频谱显示全局错误 |

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

**FREQ/POWR —— 16 字节头 + 数据：**

```
偏移  大小  类型    内容
0     4    bytes   magic: 'FREQ' | 'POWR'
4     4    u32     version（FREQ/POWR 配对序号）
8     4    u32     points（点数）
12    4    f32     sweep_ms（SDK 估算的 SWP 最短扫频时间，ms；无估算时为 0）
16    ...         数据
```

| magic | 数据类型 | 说明 |
|---|---|---|
| `FREQ` | float64 × points | 频率轴（Hz），仅在版本变化时发送 |
| `POWR` | float32 × points | 功率迹线（dBm），与同版本 FREQ 配对 |

原生 SDR 的 RTAF 保留显示窗口内全部 FFT 频点（最多 16240），原生 RTA 保留设备返回帧宽。浏览器的迹线、标记与导出保留原始 Hz 和功率，不插值制造频率细节。本地缩放不发送采集命令，也不能提高物理 RBW。余辉使用锚定显示窗口 Hz 的 2048 个粗频率列；缩放只裁剪已有余辉，不重建历史。SWP/RTA/SDR 瀑布历史保留原生频点位置，每行最多 16384 列、最多 512 行；更大输入在此上限内按峰值聚合。除用户启用 SWP GapFill 外，缺失频点保持空隙。源频率网格改变时清空累积，避免保持值移到错误频率。

SWP/RTA/SDR 栅格绘制按设备像素列保留首点、最低点、最高点和末点，保持原始频率及源顺序，并在每个缺失频点断开。缩放后原生频点分占像素时会显示完整细节；标记和导出始终使用完整源数组。可选 SWP 自适应平滑仍使用完整帧，与标记读数保持一致。

全局导航缩略图在有界列内按原始 Hz 绘制独立最低/最高线段；含缺失样本的列保持空白。导航只概览已保留数据，不增加分辨率。




**RTA2 / RTAF —— 24 字节基础头：**

```
magic(4) + ver(u32) + points(u32) + wfLen(u16) + maxDensity(u16) + startHz(f64)
+ float64[points] + float32[points] + uint16[wfLen] + stopHz(f64)
```

RTA 发送 RTA2：基础载荷后追加 ASCII TRIG、小端 u32 JSON 字节长度和 UTF-8 触发元数据（见下文）。SDR 全景仍发送不带尾段的 RTAF。浏览器可读取旧 RTAF 帧，但不能据此判断硬件采集完成；旧客户端需刷新以支持 RTA2。points 为显示迹线长度。

#### 2.3 原生接收订阅

| 端点 | 二进制输出 |
|---|---|
| `/ws` | FREQ/POWR/RTA2/RTAF 显示帧及 JSON 命令/状态 |
| `/ws?iq=1` | 主路 IQBF |
| `/ws?iqaux=1` | 辅路 IQAX |
| `/ws/audio?channel=1` | AUD2 接收 PCM |
| `/ws/decode?channel=1` | SDE2 接收事件 |

音频/解码的 `channel` 可省略以订阅全部频道，或选择槽位 1–3；非法值返回 HTTP 400。这些订阅不携带显示帧。后端负责解调，浏览器负责 PCM 播放率转换及结果显示。当前产品只提供主路控制，进入 SDR 时禁用辅路；后端订阅支持三路。

流身份为 `(epoch, channel_id)`。重调分配新频道 ID；重建计划、重新采集和新会话分配新 epoch。拒绝请求保留旧配置及旧流。旧输出尾帧先于新起始边界。每个音频/解码订阅者拥有从零开始的序号，不重置接收器或其他订阅者。源序号断裂或无起始标记的格式变化均显式失败。

流使用每客户端最多 64 条消息的有界 FIFO。溢出时以代码 1013 关闭慢订阅，不通过丢弃样本或事件维持播放。显示迹线只保留最新帧。处理职责见 [ARCHITECTURE.md](ARCHITECTURE.md)，接收限制见 [KNOWN_ISSUES.md](KNOWN_ISSUES.md)。

共享原生接收器工作线程故障会停止全部音频和解码接收器。显示/控制流收到 `RECEIVER_ERROR`，内容为 `{error, failed:true, scope:'all_receivers'}`。STATUS 在 `sdr.receiver_error:string|null` 中保留诊断。音频和解码订阅以代码 1011 和原因 `receiver failed` 关闭。采集、PFB、fine DDC、原始 IQ、频谱和瀑布继续运行。接收器故障不设置 `last_error` 或 `ddc_error`，不显示频谱错误遮罩，也不改变硬件连接状态 `connected`。采集和设备故障由全局错误流程处理。

原始 IQ 和频谱处理使用有界队列背压。队列积压时等待，直到明确失败，不重新打开设备。SDK 持续停滞时，以原始原因结束采集；恢复须创建新的采集 epoch。

调谐、滤波器编辑和缩放仍可使用。恢复须发送显式包含 `mode` 或 `demod` 的解调器或频道命令，重复选择当前模式也可重试。仅修改频率或滤波器的命令（如 `SET_SDR_DEMOD {ifbw}`）会保留故障。停止的工作线程全部退出且发布器关闭失败订阅前，恢复请求返回 `code:'receiver_retiring'`，须稍后重试。服务不会安排自动重启，也不会无上限创建替代线程。

成功恢复后清除 `sdr.receiver_error`，为每个已启用频道分配新的流身份和起始边界。客户端须先清除旧 PCM 和解码状态，再接收新流。接收器故障时无法保证尾帧完整有序；缺失 IQ 区间两侧的样本不会拼接。

音频和解码控件就地显示错误。关闭浮动解码窗格后，Decode 行仍保留错误。通过现有解调器或频道控件重试。只有 STATUS 返回 `sdr.receiver_error:null` 且已启用频道的流身份已更新时，浏览器才清除故障。

AudioContext 创建或恢复错误只影响播放。过期工作线程回调被忽略。本地解码或初始化故障保留到显式重试，或收到健康的新流身份为止。

#### 2.4 IQBF/IQAX v2 头部

所有多字节二进制字段均为小端。IQ 使用归一化 ADC 满量程单位（`1/32768`），封帧不做 AGC 或削波；滤波过冲可超出 `[-1,1]`。

| 偏移 | 长度 | 字段 |
|---:|---:|---|
| 0 | 4 | magic：`IQBF` 或 `IQAX` |
| 4 | 4 | version，u32 = 2 |
| 8 | 4 | seq，u32；0 为空起始标记 |
| 12 | 4 | samples，u32，复数样本数 |
| 16 | 8 | rate，f64，输出 Sps |
| 24 | 8 | center，f64，Hz |
| 32 | 4 | channel_id，u32 |
| 36 | 4 | epoch，u32 |
| 40 | 8 | input_index，u64，采集复数样本 |
| 48 | 8 | group_delay_samples，f64，输出复数样本 |
| 56 | 8×samples | 交错 I/Q，f32 |

`input_index` 是首个输出样本在计划输入中的因果位置，不是包首索引；延迟单独传输。对齐时间为 `input_index / input_rate - group_delay_samples / rate`。原生路径提供精确索引；合成源使用自身采样时钟及零延迟。离线参考 fixture 可用 `u64::MAX` 表示时钟不可用，不得据此对齐。u64 必须保持整数精度（JavaScript 使用 `bigint`）。

`STATUS.channel_registry`、`actual.channel_registry` 与 `sdr.actual.channel_registry` 共用快照。每项含 `channel`、`channel_id`、`epoch`、`frequency_hz`、`output_rate`、`input_rate`、`group_delay_samples`、`state="active"`。禁用频道不在表中；离开 SDR 后注册表与频道时钟均为空。服务不协商 v1。

#### 2.5 AUD2 音频头部与时钟

AUD2 使用 96 字节头，后接有限交错 f32 PCM；总字节数为 `96 + frames * channels * 4`。

| 偏移 | 类型 | 字段 |
|---:|---|---|
| 0 | 4 bytes | `AUD2` |
| 4 | u32 | version = 1 |
| 8 | u32 | seq；0 为空起始标记 |
| 12 | u32 | PCM 帧数（一帧每声道各一个样本） |
| 16 | f64 | 实际 PCM 率，Hz |
| 24 | f64 | 调谐频率，Hz |
| 32 | u32 | channel_id |
| 36 | u32 | epoch |
| 40 | u64 | input_index，采集复数样本 |
| 48 | f64 | delay_seconds，有限非负；总延迟未知时为已知部分 |
| 56 | u32 | channels，1 或 2 |
| 60 | u32 | 低 16 位：format = 1（f32）；bit 16：总延迟未知；bit 17：因果播放时钟 |
| 64 | u64 | audio_index，PCM 帧位置 |
| 72 | f64 | input_rate，采集 Sps |
| 80 | u64 | input_fraction_num |
| 88 | u64 | input_fraction_den |
| 96 | f32 array | 交错 PCM |

采样率有限且为正，ID/epoch 非零，分数满足 `0 <= num < den`、`den >= 1`。总延迟已知且时钟对应源样本时，对齐时间为 `(input_index + num/den) / input_rate - delay_seconds`。未知延迟或因果播放时钟不能证明精确源对齐。相邻协议块可共用输入位置或跳到下一协议帧；用 `audio_index` 检查 PCM 连续性，不按 PCM 时长推造输入位置。延迟可逐帧变化。

重置 NR 尾部时，AUD2 源位置可跳过被丢弃的降噪样本，而 `audio_index` 保持连续。此类源位置前移不允许样本区间重叠或倒退；后两者仍会被拒绝。

新订阅者收到空 `seq=0`，保留首个实际块的全局 `audio_index`（可非零），然后从 `seq=1` 开始。codec 实际率/声道变化可在同一 IQ 身份内发送新起始帧：`audio_index` 归零，下一序号为 1，播放器清空旧 PCM 并重建播放率适配器，codec 状态继续有效。模式切换后旧 PCM 静音至新边界。

播放反馈属于浏览器内部消息，不是 WebSocket 确认接口。入口发送 `{type:'samples', samples, stereo, delivery, generation}`；播放端以 `{type:'credit', generation, limit}` 授予累计容量，以 `{type:'accepted', generation, delivery}` 确认接收。仅匹配的 generation 生效。未发送与未确认帧共用三秒交付预算，溢出显式失败。接收模式决定缓冲策略：模拟模式启播量 450 ms；DRM/DRM+ 启播量 1.6 s、队列上限 2.4 s。

#### 2.6 SDE2 解码事件

| 偏移 | 类型 | 字段 |
|---:|---|---|
| 0 | 4 bytes | `SDE2` |
| 4 | u32 | UTF-8 JSON 字节数，1 至 2 MiB（含端点） |
| 8 | UTF-8 | JSON 信封，长度必须与声明一致 |

信封字段为 `version=1`、`channel`（槽位 1–3）、`channel_id`、`epoch`、`seq`、`input_index`、`input_rate`、`input_fraction_num`、`input_fraction_den`、`delay_seconds`、`delay_known`、`causal_presentation`、`timing_kind`、`mode`、`event`。三个 u64 位置/分数字段使用十进制字符串。时钟单位和标志与 AUD2 相同。`timing_kind` 为 `sample`、`protocol_provenance` 或 `causal_presentation`；协议来源位置不代表恢复了样本时间戳。FT8 结果保留搜索窗口时刻，不使用异步完成时刻。`event` 携带模式结果；序号零携带 `{type:'start'}`。

AM 按源样本时钟的 20 Hz 网格发送 `event.type:'am-info'`，不随浏览器绘制或音频回调块大小变化；SDE2 信封沿用上述源时钟与延迟标志。事件字段如下：

| 字段 | 类型与含义 |
|---|---|
| `amSyncGeneration` | u32；实际接收器配置的代次，对应 STATUS 的 `am_sync_generation` |
| `syncEnabled` | 布尔值；实际同步检波状态，不是待确认的请求 |
| `syncLocked` | 布尔值；同步检波已开启，且载波功率、相干性、相位与频率一致性检查持续通过 |
| `carrierHz` | 数值或 null；恢复的绝对 RF 载波频率（Hz），由实际接收频率加 `carrierOffsetHz` 得到；缺少有效载波证据时为 null |
| `carrierOffsetHz` | 数值或 null；恢复的载波频偏，单位 Hz；两种检波模式下只要载波证据有效均可提供 |
| `modulationPct` | 数值或 null；AGC 前按源时钟窗口计算 `100 * sqrt(2 * variance(envelope)) / mean(envelope)`，表示等效单音 RMS 调制度，不是节目峰值调制度 |
| `inputDbFs` | 数值或 null；原始 IQ 电平 `10*log10(mean(I²+Q²))`，单位 dBFS；零功率时为 null |
| `rmsDbFs`、`peakDbFs` | 数值；最终 PCM 的 RMS 与峰值电平，单位 dBFS，静音下限为 -160 dB |
| `scope` | 最多 256 个实际最终 PCM 样本，包含监听处理、降噪与增益的结果 |

无法确定载波频率、频偏或调制度时使用 null，不以零代替；深度载波衰落会使这些值失效。AM 不提供 SNR。关闭同步检波时，即使载波指标有效，`syncLocked` 也为 false。恢复链路先粗略捕获频率，再通过带积分的相位环跟踪；捕获与失锁分别要求持续 150 ms，深衰落立即使结果失效。输入功率采用 100 ms 窗口与 500 ms 指数平滑。包络方差先在每个 100 ms 窗口内归一化，再以 1 s 时间常数平滑调制功率，避免较慢的 RF 衰落被误算为调制度。捕获后频偏采用 250 ms 平滑。音频波形仍为实际 PCM 样本。

浏览器仅接受 `amSyncGeneration` 与权威 STATUS 代次一致的 AM 指标和锁定状态；代次未知时不接受。即使关闭后再次开启、布尔值恢复相同，也不会误用先前切换前排队的事件。设备或控制连接断开会使指标与代次失效，只有 STATUS 确认连接后才恢复解码。

WFM 事件 `wfm-info` 中的 `carrierHz` 是该接收流的频道中心频率（来自信道化器流身份），不是独立测得的载波估计；其余字段（频偏、导频、RDS、波形）为实际解调测量。

SSTV 订阅者先收到起始事件及当前图像的有界快照，再收到实时增量行。图像事件含 `type:'sstv-image'`、`width`、`height`、`row`、`rows`、紧密排列 RGB 字节数组 `rgb` 和 `meta`；快照每块最多 64 行。服务只保留当前图像，历史由浏览器保存。
图像边界保持有序。重新固定 SSTV 模式时使用全新画布；长时间静音的计时保持有界。

浏览器以 `{type:'frames', envelopes}` 按 FIFO 交付事件批次，应用后通过内部 worker 端口回复 `{type:'ack'}`。排队和在途数据合计上限为 32 事件 / 4 MiB；溢出显式失败。此确认不发送到 `/ws/decode`。

> **频谱细节**：POWR 使用 float32 电平和以 Hz 表示的原生 FREQ 轴。RTA/SDR 为迹线、标记和导出保留原生频点；本地缩放不发送采集命令，也不改善物理 RBW。密度可跟随当前处理后的迹线，也可积累 Average/Max Hold 前的实时频谱帧；切换来源清除历史。余辉按真实时间半衰期衰减（Off、0.5、2、5、10 秒），命中权重按经过时间计算，并限制长间隔的影响。Off 停止累积与显示。图层选择只改变可见性，不改变迹线模式、marker、测量和导出的数据来源。密度显示时取消迹线宽辉光。对比度调整颜色，透明度独立映射。瀑布保留原生频率位置；缺口保持为空，除非启用 SWP GapFill。

频率轴版本切换时，优先接收新轴的首帧，拒收旧版本帧。密度按原生频点中心投影，将幅度贡献按比例分到相邻两行。显示宽度为 1/3/5/9 CSS 像素，默认 5，仅扩大已有幅度格中心的垂直绘制范围，不改变命中历史、幅度、频率或测量。对比度调整颜色，透明度独立映射；浅色主题使用饱和色，实测底噪以较低强度保留。Grain 默认为 128，NaN 缺口保持空白；Off 清除累积，跟随迹线源在 VIEW 时冻结，实时源继续积累。切换宽度、主题和对比度均保留历史。

CSV 导出当前模式所选处理后迹线及匹配的原生频率轴；OFF、缺失或长度不匹配时提示无数据，不能回退到旧 SWP 迹线。Limits 与频道测量仅使用所选的处理后绝对功率迹线；归一化相对迹线显示 N/A，不生成绝对测量 CSV 行。RTA/SDR 禁用这些测量控件。dBc 不叠加外部电平偏移。SWP 瀑布从已接受的原始 POWR 帧取行，最高 10 Hz，重绘本身不会生成新行。


---

## 3. 测量模式

### 谐波（Harmonic）

1. `SET_MODE {mode:'harmonic'}` → `SET_HARM {f0, count, span}`
2. 服务端逐谐波自动调谐（H1~H5…），每次结果推送 `HARM`

### 相噪（Phase Noise）

1. `SET_MODE {mode:'pnm'}` → `SET_PNM {center, threshold, traceavg, start, stop}`
2. 服务端增量采集（`PNM_GetPartialUpdatedFullTrace`），每帧推送 `PNM`（含 progress）

每次谐波/相噪 Set 携带新的 `session_id`；成功命令 STATUS 回传该标识，HARM/PNM 结果携带实际应用会话的标识。客户端拒绝旧会话结果，包括重配置时仍在结束的旧硬件采集周期。浏览器在 Set 时清除旧结果；PNM 有独立、可见的平均控制。

### SDR（软件无线电接收）

1. `SET_MODE {mode:'sdr'}` → 可选 `SET_SDR {center, decimate}`（捕获窗口）
2. `SET_SDR_TUNE {listen}` 调谐主接收器（原生后端按需重新采集并保留辅路频率）；`SET_SDR_CHANNEL {channel:1, freq}` 使用相同原生调谐行为。`SET_SDR_DEMOD` 选择解调器与音频设置
3. 服务端推送 `RTAF`（全景频谱 + 瀑布）与订阅的 `IQBF` 基带。原生接收音频使用 `/ws/audio`，解码事件使用 `/ws/decode`。电平/静噪/ADM 指标在 `STATUS.sdr`。

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
                        freq = struct.unpack_from('<%dd' % pts, msg.data, 16)
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
- **设备调用串行**：原生 SDK shim 持有 session 线程并串行访问设备。
- **专有二进制**：`libhtraapi.so` 需从 HAROGIC 官方获取（`htra_api.py` 已随仓库附带）
- **SystemClockSource=External**：危险配置，勿用（会导致设备挂死）


## 触发（Trigger）参考

SET_TRIGGER 共十一项参数。请求保存在 `req.rta.trigger_<字段名>`；`req.rta.trigger_actual` 分别报告 SDK 实际应用的 source、edge、level、safetime、delay、pretime、acqtime、retrigger、retriggerperiod、out、outpolarity 及运行元数据。实际配置来自 ProfileOut，poi 来自 SDK FrameInfo；缺少依据时保留 unknown/null，不能以请求值代替。

RTA2 的 TRIG 元数据包括 valid、supported、simulated、generation、frames、capture_id、packet_index、packet_count、waiting、complete、trigger_missed、last_sdk_status。generation 随成功配置递增；frames 统计成功取得的 SDK 包，而非 STATUS 次数。SDK 没有采集标识时 capture_id 为 null。packet_index 从 BusStart 后的首个成功包按 1 计数，连续性丢失后为 null；packet_count 仅 FixedPoints 已知。waiting 无 SDK 依据时为 null，因为 -10 既可能是等待触发，也可能是通信错误。

无重触发的有限 Level/FixedPoints 采集，须保持数据包连续，并在最后一个配置包或 SDK 返回 -301/-303 时才报告完成；丢失数据后，即使 -301/-303 也不能证明完整捕获。Timer、自适应及重触发采集缺少完整边界依据时仍为 null。-302/-303 标记 trigger_missed；-303 保留完整捕获并保持最终数据，同时提示漏触发。前端处理有效 complete 对应数据包后才冻结，并丢弃旧 generation；不得以首帧或固定延迟猜测完成。有限 Level 采集在包内及包间保留最大频谱。模拟 RTA 明确报告 unsupported、invalid，不产生硬件 HIT。时钟重新锁定状态 -36..-39 保留有效 RTA 数据和 last_sdk_status，Auto 将其视为告警/等待。

确认采集完成后，服务端停止读取 SDK，保留当前 generation 的完整 RTA2，并以 5 Hz 重发，供稍后连接的订阅者接收。重发不增加 frames、packet_index 或 Auto 观测次数。成功重配置、重新就绪/Free Run、回滚重启、会话失效或退出模式会清除保留帧。捕获保持由服务端负责，不能据此认为有限采集的 SDK 已停止出包。

标准 SWP 在浏览器中检测相邻显示迹线的门限交叉，仅启用电平、边沿及 Capture 辅助操作；RTA 使用设备时序。SDR、谐波与相噪模式禁用整组触发控件。API 的 external/timer 状态仅回读显示。Free Run/Escape 解除保持；成功换模或 Preset 清除本地保持。进入 RTA 选择 bus；Auto 不解除已就绪触发。

## Auto Scale 与本地电平控制

显式 Auto 最多运行 20 秒、24 步，每步须取得同模式、同配置的新数据。SDK IF 过载（-12）时，从当前实际配置 Ref 每次上调 10 dB，最高为 caps.ref_max；若原值已更高，不向下回调，也不修改增益选择。等待触发或普通无数据不会抬高 Ref。只有干净频谱可用于拟合，新目标还须由新的干净数据确认；恢复后不将硬件 Ref 降回已证实的过载区。普通 SDR 拟合只改显示 Ref；显式 Auto 遇过载可经正常准入重配 IQS。自动进入拟合携带 display_only:true，绝不改硬件。没有后台过载恢复。

auto_ref.result 为 idle、adjusting、applied、ok、no_data、overflow、limit、stalled、failed、cancelled 或 timeout。只有成功结束时 target 非 null。始终没有可用采集时以 no_data 结束；已有调整但未获得新干净数据确认时以 timeout 结束。手动 Ref/AMP、调频、换模和 Preset 会取消会话并清除旧 target。

SWP/RTA/SDR 的手动 Ref 使用独立本地窗口（-200 至 +100 dBm 加外部 Offset）。SET_REF 和 AUTO_SCALE.current_ref 减去 Offset 后限制在现有 -160 至 +40 dBm 协议范围。匹配的原始请求回读接受窗口，不替换显示值：Offset 为零时显示 +100 对应硬件请求 +40。同一限幅目标下继续移动窗口不重配硬件，也不重启待确认超时。拒绝或超时恢复已接受的显示值。SDR 的 sdr.ref 确认未补偿请求，与有效 ref 分开。应用 Auto 目标时只加一次 Offset。修改 Offset 同步平移绝对纵轴和迹线，不发送硬件命令；相对 dB 不变。瀑布 Fixed 范围须为有限数且低值小于高值；非法编辑回退并提示。映射或有效范围变化会清空量化历史，暂停时亦然。std/rta/sdr 可使用本地 Scale/Unit/Offset/瀑布设置；独占谐波/相噪测量时禁用 Level 控件。
