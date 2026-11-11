# 带宽内扫描与识别

信道工作台旁的“识别”打开同款浮窗，不自动启动扫描。开始/暂停控制共享的连续扫描；隐藏、收起和弹出只影响视图。选区识别可单独使用，横向频段同时显示在频谱和瀑布上，松手提交，Esc 取消；瀑布与冻结图均识别当前 IQ，不回放历史 IQ。`I` 仍用于 IQ 录制。

发现使用独立 65536 点 Hann、四段 50% 重叠 Welch 线性 PSD，默认 5 次/秒。连续性错误或资源压力下跳过整块、降速或暂停；没有全带连续短突发捕捉承诺。频道表按频率窗口积分线性功率，至少需要 8 个 FFT bin；不足时缩小实际采集带宽，显示放大不增加分辨率。高于本底是检测裕量，不能当作 RF SNR 或未经标定的 dBm。观测命中率只统计实际观察。

识别优先旁路兼容、解调前的既有基带 IQ，否则借用一个空闲非 Current 槽。较宽的旁路 IQ 仍通过既有计划 DDC 做本地区域滤波和降采样，不能把邻频信号交给特征分析；分析 worker 收集约 0.8 秒 IQ，限制为 2 秒或 8 MiB，并按已测线程 CPU 时间限制后台识别占空比。候选不占正式名额，停用的正式定义仍保留名额。临时 DDC 使用同一几何/成本准入，正式创建前等待同步有界退役回执，失败时不复用未释放的槽。满 16 路仍可发现或复用 IQ，其他识别显示等待资源。

首批形态为载波、AM/FM 类、2FSK/4FSK 类、PSK 类或未知；不自动把调制形态命名为 DMR、P25 等协议。FM 19 kHz 导频是专用特征；已运行解码器的有效 RDS 组、DRM FAC、FT8 消息可提供独立协议证据。不会批量启动 provider。`label` 表达调制形态，`protocol` 只来自专用结构化证据；`receiver_available` 表达建议接收模式可用性，`decoder_available` 单独表达已确认协议的解码器可用性。接收建议变化会推进候选代次，普通电平更新不会。可信状态不是准确率；不确定结果可加入 IQ 信道，明确选择音频模式后可监听。加入与复用继续走接收机预检、身份及资源检查。

## HTTP

- `GET /api/sdr/scan`：共享会话、源身份、范围、最多 128 行摘要、探针占用及观察/CPU/跳过统计。候选上限 256，识别队列上限 32。摘要不包含详细证据。
- `GET /api/sdr/scan/signals/{id}`：按需读取特征、协议证据、实际 IQ 来源及采样时间。时间为服务接收时的主机时间，不能当作已校准 RF UTC 时间。
- `POST /api/sdr/scan`：`start`、`pause`、`stop`、`configure`、`identify`、`identify_region`、`cancel_region`、`ignore`。

所有命令使用当前 `service_id`、唯一 `request_id` 和十进制字符串 `capture_epoch`。除 `identify_region`/`cancel_region` 外，还要求 GET 返回的 `scan_id` 和 `expected_revision`。`identify`/`ignore` 额外要求 `signal_id`、字符串 `signal_generation`。成功回包带 `request_id` 与最新快照；同一 ID 和相同内容重试返回原回执，不同内容拒绝。冲突返回 409，错误字段为 `code`、`error`、`request_id`。

`identify_region` 示例（身份须从当前 GET 读取）：

```json
{"type":"identify_region","service_id":"…","request_id":"unique-id","capture_epoch":"…","low_hz":9830000,"high_hz":9840000,"origin":"waterfall"}
```

`origin` 为 `spectrum`、`waterfall` 或 `manual`，数据源始终为 `live_iq`。请求后收集新的连续观察块，最多分解 8 个区域；新选区替换临时组，不修改连续扫描范围或隐式启动扫描。越过有效采集边界会拒绝，不能静默裁剪或调谐。服务、采集几何、校准尺度变化会使旧结果失效；取消和新选区阻止迟到回包覆盖新状态。

`configure` 仅修改提供的字段：`range: {low_hz,high_hz} | null`、`channels: [{low_hz,high_hz}]`（最多 256）、`threshold_db`（5–20）、`auto_identify`。没有频道表时为区域发现。正式接收机创建可携带 `scan: {capture_epoch,signal_id,signal_generation}`，服务在预检及提交时复核推荐来源。

Fake 模式只有独立基带回放，没有共同的 RF 采集 IQ，因此不提供虚构扫描；扫描算法在单独合成测试中验证。实机准确率、长时间 RF 和高负载能力需按实际设备、速率及接收配置验收。

## 验证

`cargo test --release --manifest-path backend/Cargo.toml --bin websad` 与前端 Vitest 覆盖算法、命令、资源和选择状态。`tools/e2e/signal_scan.mjs` 和 `tools/bench-signal-scan.py` 需要显式设置 `WEBSA_SCAN_LIVE=1`，对已运行的实机服务执行浏览器闭环和 1/8/16 路测量；产物目录必须指定在仓库外。后者只在单信道基线上运行，创建带唯一标签的测试接收机，并在结束时清理测试录制和信道。
