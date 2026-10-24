# 架构

## 系统与所有权

原生 Rust 服务负责设备配置、采集、信道化和接收 DSP。C++ shim 串行调用 HAROGIC 官方 SDK。浏览器负责控制、频谱绘制、解码结果展示和音频播放。`tools/reference/` 中的 Python 模块仅供离线数值参考和 fixture 工具使用。

```text
浏览器控制 → HTTP/WS 命令 → 原生模式所有者 → 保留的 SDK session
SDK IQ + FFT → 有界采集队列 → 有序 PFB workers → fine DDC/成帧
                                             ├→ IQBF/IQAX 消费者
                                             └→ receiver workers → AUD2/SDE2 → 浏览器
SDK SWP/RTA 迹线 → 显示发布器 → 浏览器频谱/密度/瀑布
```

SWP、RTA、SDR、谐波和相噪各自保存配置，共享一个物理设备 session。STATUS 发布设备实际值和能力。命令校验及候选计划准入在替换活动状态前完成；拒绝请求保留原计划和原流。

## 设备 session 与采集

一个长期运行的 SDK 管理线程负责 SWP/RTA/IQS/PNM，所有 SDK 调用均串行执行。设备正常时，模式切换只停止采集并释放模式状态，保留物理句柄。`Device_Open` 使用长期会话中地址稳定的 `void**` 句柄槽：厂商 PNM worker 会保留该地址，直到物理连接关闭。失效或断链句柄不得传给 `Device_Close`。

IQS 配置与启动采集（arm）分开。服务先准备 PFB、fine DDC、接收器工作线程和元数据，再显式启动采集；恢复时也遵循此顺序。首包予以保留，并校验包序和硬件时间戳。主机样本索引不能替代对硬件连续性的核对。

IQS 返回 `-12` 时，若包含有效 IQ 包，服务保留样本及时间戳，通过 `status_warning=-12` 单独报告饱和。未显式启动 Auto 时，饱和保持手动 Ref，也不会把有效包计为总线失败。显式 Auto 恢复有明确上限，且只使用同一模式、同一配置下的新观测，详见 [MODE_STATE_FLOW](MODE_STATE_FLOW.md)。IQ、FFT、告警和计时经同一次 `san90_next_v3` 返回；例行元数据轮询不消费 IQ。畸形包和真实时间戳断裂显式失败。

定期获取 GNSS 状态时使用 SDK 缓存快照。设备和可选时钟能力根据成功打开设备及读取硬件状态的结果确定；缺失数据保持未知。外部参考配置为 10 MHz。恢复时显式将物理会话标记为失效；退出时先结束模式管理逻辑，再关闭会话并等待 SDK 线程结束。

## SDR 信道化与接收器

### 阶段映射

下表是当前实现路径。`tools/checks/check_doc_paths.py` 检查表内具体路径。

| 阶段 | 模块 |
|---|---|
| 设备 session 与采集 | `backend/sdk/san90_sdk.cpp`, `backend/src/bin/websad/service_capture.rs` |
| 服务、命令与发布 | `backend/src/bin/websad.rs`, `backend/src/bin/websad/service_ddc.rs` |
| 粗 PFB 与有序 workers | `backend/src/pfb.rs`, `backend/src/pfb_pool.rs` |
| fine DDC 与样本时钟 | `backend/src/fine.rs`, `backend/src/narrow.rs`, `backend/src/rate_plan.rs` |
| 流水线编排与准入 | `backend/src/bin/websad/sdr_pipeline.rs`, `backend/src/bin/websad/service_preflight.rs` |
| 原生接收器编排 | `backend/src/bin/websad/receiver_workers.rs`, `backend/src/receiver.rs` |
| 模拟与数字 DSP | `dsp/core/src/analog/`, `dsp/core/src/digital/`, `dsp/core/src/pipeline.rs` |
| 原生降噪 | `backend/src/nr.rs`, `dsp/deepfilter/src/` |
| 二进制协议与订阅边界 | `backend/src/wire.rs`, `backend/src/bin/websad/service_stream.rs`, `backend/src/bin/websad/subscriber_stream.rs` |
| 浏览器流与解码展示 | `frontend/src/sdr/iqStream.ts`, `frontend/src/sdr/decodeWorker.ts`, `frontend/src/ui/decodeWindow.ts` |
| 播放 | `frontend/src/audio/sdrAudio.ts`, `frontend/src/audio/sdrAudioWorker.ts`, `frontend/src/audio/sdrAudioWorklet.js` |
| 离线参考与 fixture | `tools/reference/`, `tools/fixtures/gen_dsp_fixtures.py`, `tests/fixtures/dsp/` |

### 粗 PFB 与 fine DDC

全带宽采集输入为 62.5 MSps，可用带宽为 50 MHz。共享二倍过采样 PFB 使用 128 个频率箱，步长为 64 个样本：粗频率箱间距为 488.28125 kHz，每箱输出采样率为 976.5625 kSps。窄带设备采集会相应调整这些参数。频率箱数量、间距和输出采样率是不同的量，均不代表产品提供的接收器数量。

原型滤波器支持每相 4、8 或 16 个抽头，服务默认使用 16 个。通带和混叠抑制取决于抽头数及频偏。fine 计划必须落在所选粗频率箱的可用通带内；不能截断宽带信号后仍将其视为等价接收。超出当前频道可接收频率范围的宽带模式会被明确拒绝。

每路接收器独立保存频率、模式、占用带宽、`RatePlan`、振荡器相位、滤波历史和输出率。fine DDC 频移后执行多级抗混叠抽取，必要时在接收器输入适配精确协议率。播放采样率不是所有模式的 IQ 率下限。USB/LSB 包络覆盖边带上限，CW 包含 pitch，FT8 至少保留正频率 3.2 kHz，SSTV 至少保留正频率 3 kHz；DRM 和 DRM+ 使用协议带宽。

主频道调谐超出采集范围时，先验证所有已开启频道，再移动共享采集中心。辅助频道不能独立移动中心。拒绝配置保留所有活动频道。在当前计划内可完成的变更尽量只更新目标 fine 接收器。

### Workers、准入与背压

原始采集队列容纳 64 包。PFB 使用四个固定 worker，最多六个任务在途，按输入顺序收集结果。fine/成帧交接容量为二。两个固定 receiver worker 独占频道可变 DSP 状态；另一个 FT8 worker 执行搜索，避免阻塞实时监听。DFN 模板在采集前准备，各接收器独占可变处理状态。

后端支持三个独立配置槽。当前产品界面仅提供主路控制，进入 SDR 时停用辅助槽。粗频率箱数量和内部基准测试的路数不代表产品提供更多频道。混音、录制及动态频道数尚未实现。

准入在更改采集、身份或注册表前检查频道数、合计输出率和 fine/成帧成本。fine/成帧校准在采集前完成，进程内复用。成本策略保留二倍余量：`2 × sum(每粗样本成本 × 粗采样率) <= 1e9 ns/s`。这是内部保守预算，不证明 SDK、PFB、接收器、网络或录制容量。

对于显示帧，慢客户端的旧帧可由新帧替换。有序 IQ、PCM 和解码输出使用有界队列，溢出时明确报错；不得通过静默丢样、扩大队列或阻塞采集来通过吞吐检查。音频容量反馈包括排队和未确认的 PCM；解码批次保持有界，按 FIFO 顺序应用后确认。协议和反馈契约见 [API](API.md)。

Receiver worker 失败会终止当前 SDR 会话。服务保留首次诊断及 worker 计数器，回收失败的 worker 和采集，仅发送一次 `receiver_failed` 错误，并以接收器失败原因关闭 PCM/事件订阅；不会恢复不连续的流，也不会重复轮询已锁存的 worker 故障。健康重配置仍按顺序发布旧流尾帧；失败会话不保证完整尾帧。显式离开 SDR 再返回会创建新 worker 和流身份，硬件链路状态保持独立。

DRM 在 SDC 尚未提供复用配置时，只保留近期完整等化帧：DRM30 为 13 帧，DRM+ 为 16 帧，覆盖长交织器预热及超帧对齐。裁剪保留每帧的 FAC 标记和源样本来源；已有配置后的解码不变。选定模式的定时路径也沿用绝对样本坐标裁剪未同步 IQ 历史。这些是解码器捕获历史的边界，不是丢弃输入帧或创建新流身份；失锁恢复仍清理既有解码状态。

### 流身份与时钟

IQBF/IQAX v2 使用 `(epoch, channel_id)` 标识流。重调更换频道身份，新 DSP/采集计划更换 epoch。旧输出先于新流空 `seq=0` 边界结束。拒绝请求不分配新身份。注册表和频道时钟来自同一不可变流水线快照；退出 SDR 后清空。

IQ `input_index` 是首个输出在采集复数样本域中的因果位置，不是包首。群延迟独立传输，单位为输出复数样本。对齐公式为 `input_index / input_rate - group_delay_samples / output_rate`。合帧保留首段非空输出时钟。消费者必须精确保留 u64 索引。

AUD2 携带交错 f32 PCM、实际率和声道、流身份、PCM `audio_index`、采集输入位置及分数时刻。SDE2 携带相同身份和时钟语义的解码事件。总延迟未知和因果播放有明确标志，两者均不允许宣称精确源对齐。解码窗格通过模式标题的工具提示展示这些时钟详情，而非常规显示标题栏标签。DRM/DRM+ 的时序与电台锁定和音频播放状态无关；解码器错误仍显示在标题栏。FT8 结果保留原搜索窗口时刻。新订阅者获得自己的起始边界；晚订阅 SSTV 获得有界当前图像快照，不重置接收器。完整布局及排序见 [API](API.md)。

### DSP 与播放边界

当前模式注册表包括 AM、DSB、USB、LSB、CW、NFM、WFM、PM、FT8、SSTV、DRM 和 DRM+。原生接收器直接复用共享 Rust DSP。模拟监听可使用 AGC、静噪、增益和降噪；数字解码保留原始协议输入。监听增强不得改变 FT8/SSTV/DRM 输入。

WFM 保留立体声和 RDS。FM 分析单位为 Hz，固定 NFM ±5 kHz、WFM ±75 kHz 归一化先于自适应 AGC；关闭 AGC 不取消单位转换。立体声指示表示有导频，不保证节目左右声道内容不同。

共享 DSP 同时运行 AM 包络与载波恢复路径，`am_sync` 切换输出时平滑过渡。实时切换保留采集、滤波与重采样状态以及流时钟。AGC 前的载波与调制度分析、最终 PCM 电平和波形事件按 20 Hz 源样本时钟网格产生；浏览器显示这些结果，不从播放回调推算指标。同步检波开启与载波锁定是两个独立状态。

浏览器接收 AUD2 和 SDE2，不重复运行生产 IQ 解调器。每条播放链只有一个生产者。模式切换清空旧 PCM 并等待新起始边界；codec 实际率或声道变化也产生新音频边界。worklet ready 握手确定播放率，重采样适应设备与声卡时钟。溢出显式终止播放链。共享 crate 还支持已提交的浏览器 WASM 离线诊断产物；它们不替代原生服务构建。

## 前端状态与绘制

用户设置及后端确认的参数使用 `core/params.ts` 槽位管理：用户操作写入 desired，STATUS 确认实际值。纯客户端偏好以本地状态为准。测量结果存放在 `core/results.ts`，通过显式 setter 更新，不使用 desired/confirmed 语义。共享类型与注册表保持为叶子模块，由上层模块依赖。注册模块必须能从 `main.ts` 的导入链访问。

后端返回原生频率网格。前端为标记和导出保留以 Hz 表示的原始频率，显示时采用保峰重采样，将未覆盖的边界频点绘制为缺口。频点间距不等于 RBW。本地缩放只改变可见窗口，不重新调谐或改变接收时钟。密度和瀑布历史均有容量上限。参数槽位在高频循环外一次性读取。

频谱 CSS 盒决定画布几何，位图尺寸通过 ResizeObserver 跟随。启用瀑布时，绘制路径和 STATUS 驱动的测量 UI 刷新均保持标记表隐藏。STATUS 不能临时恢复该表，否则频谱会缩小并因位图尺寸变化而清空，直到下一次绘制再次隐藏表格。

SWP 与常规 RTA 频谱更新以 30 fps 为目标，使用一个以上次帧绘制时刻为基准的尾随定时器。SWP 仍对每个有效扫描执行迹线、瀑布和触发处理；RTA 只处理最新待处理的常规帧。首帧、频率轴变化及已布防的 RTA 捕获仍立即处理和绘制。模式切换、重新调谐、触发重置和断线会取消待处理工作。UI 重绘请求保持同步，SDR 保留既有处理及绘制节奏。

轴拖动本地预览，手势稳定后提交。手动 Ref 属于设备控制，显示刻度和偏移独立归属。增益回读来自当前模式成功配置后的 SDK 实际值；无法获取时保持 null。前置放大 `0` 表示 Auto、`1` 表示 Off，不能据此宣称物理放大器已开启。RTA 捕获依据随 RTA2 帧传输的设备触发元数据；SDR 继续使用 RTAF，旧格式解码仅供兼容。收到帧或等待一段时间都不能证明捕获完成。SWP 在前端根据相邻扫描判断电平交叉。参数、Auto、迹线、测量与触发约定见 [MODE_STATE_FLOW](MODE_STATE_FLOW.md)。

## 质量边界与验证

后端三个槽不表示 UI 提供三路接收器，也不表示无限监听或录制。FT8 在支持的搜索窗内处理标准类型 1/2；SSTV 不解码图像后的 FSK 呼号。RDS 文本质量和短段/循环 DRM codec 播放仍有约束。[KNOWN_ISSUES](KNOWN_ISSUES.md) 记录当前限制及 SDK 边界。

浏览器 SWP/RTA 的 30 fps 调度目标取决于输入帧率与绘制开销，不代表已测硬件吞吐，也不改变后端采集时序。这些描述归纳实现；维护文档不构成重新验收硬件。

可复现入口包括：`make wasm-test` 验证共享 DSP，`make ci` 执行本地无硬件门禁，`make e2e-fake` 验证原生 fixture UI。`tools/bench/sdr_pipeline_check.py` 和 `sdr_pipeline_controls.py` 检查 live IQv2 连续性及控制；原生 `websad --sdr-pipeline-bench` 覆盖流水线。命令选项及依赖见 [tools](../../tools/README.md)。硬件验收必须核对内容、硬件时间戳、队列/失败增量和排空后的样本守恒；假源吞吐或短窗口不能证明持续 RF 运行。浏览器检查与安静的硬件性能窗口分开，见 [DEVELOPMENT](DEVELOPMENT.md)。

## 参考与许可证

项目代码使用 MIT 许可证。厂商 SDK 二进制为专有软件，需另行获取。DSP 技术参考 liquid-dsp、tpt-dsp、sdr-web、Radioband 和 pffft.wasm；参考不表示复制其实现。离线数值一致性以本仓库参考内核及已提交的 fixture 为准。

FT8 协议表来自 [ft8_lib](https://github.com/kgoba/ft8_lib)（MIT），来源见 `tools/fixtures/port_ft8_tables.py`。SSTV 包含 `dsp/core/src/digital/sstv/vendor/` 中 MIT 许可解码器。CW 使用 `tools/vendor/ggmorse/` 中的 [ggmorse](https://github.com/ggerganov/ggmorse) 并保留许可证。DeepFilterNet 集成保留 `dsp/deepfilter/` 下的许可证。codec 构建、来源和许可边界保留在 `dsp/core/fdk/README.md`、`dsp/core/xaac/README.md`、[DRM_BENCH](DRM_BENCH.md) 和 [DRM_AAC_RESERIALIZATION](DRM_AAC_RESERIALIZATION.md)。修改 fixture 或 vendor 代码时保留归属信息。
