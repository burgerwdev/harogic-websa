# 接收机参考

[文档目录](../INDEX.md) · [协议](API.md) · [录制](RECORDINGS.md)

本文描述生产接收机接口。仅用于 fixture 的场景和实验 v1 归档单独标明，不代表生产能力。

接收机快照包含 `created_at_ms`，为服务创建逻辑 UID 时记录的 Unix 毫秒时间戳，以十进制字符串传输。
改名、调谐、停用及恢复均保留原值；新建 UID 记录自己的时间。旧服务可能省略该字段，客户端显示时间未知，
不使用连接时间或槽位编号推测创建时间。

## 采集与调谐

采集快照将 `state:running|idle|failed` 与设备连接状态分开；`parameters_state:live|last_known|unknown` 分别表示当前源参数、保留的上次参数及源尚未打开。未知时带宽／输入率为 null，`center_hz` 仅为请求中心，epoch `"0"` 为哨兵，不表示活动流；独立基带夹具标记 `parameters_state:simulated`，共享显示窗口／率为模拟值，epoch `"0"` 为哨兵，真实基带输入以各路 clock 为准；硬件已知时输入率取 PFB 输入率，epoch 为 DSP 源代次，不能混用设备配置代次或下游 DDC 率。失败／停止保留原参数，重开源分配新 DSP epoch。启用硬件信道的变更须有健康活动采集。旧服务可能缺少 `parameters_state`，失败后仍报告旧版缺省值。

主界面中心频率同时移动共享采集与 Current。主调谐、模式和共享 PCM 控制固定作用于临时源（内部槽 1）；已保存信道在工作台独立编辑。十六路容量由 Current 加最多十五条已保存信道共享；带外和主动停用都保留定义及名额。Current 不可删除，create 只分配槽 2–16。概览和管理台在未保存同频信道时将 Current 置顶；存在同频信道时隐藏 Current 并高亮所有匹配项，与编辑选择互相独立。
接收机快照通过 `coverage.state:in_band|out_of_band` 报告覆盖状态。实际采集中心或带宽变化只暂停未被完整覆盖的已保存信道，保留 `confirmed.enabled:true`、UID、频率、模式与控制设置；通带及过渡带完整覆盖后恢复原定义，主动停用的信道保持停用。显示缩放和平移不参与判断。计划内的带外中断结束当前录制片段并保留等待任务，覆盖恢复后另起片段，`coverage_interruptions` 保存中断时间；已停止任务不会自动恢复。设备或处理故障等异常中断沿用原有失败处理。


Rust 生产 IQS 路径使用版本化 shim 校验的 `62.5 MHz / capture_decimate` 源采样率；可用 RF 带宽独立限制通带，不能除以固定比例反推样本时钟。活动采集及 `last_known` 参数采用同一源率；Python 参考工具与 Soapy 适配器的采样率验证属于各自路径。

## 信道预设

`capabilities.receiver_presets:true` 开放 `GET /api/sdr/receiver-presets`，返回 `{version:1, revision:"0", presets:[]}`。每项为 `{name, settings:{purpose, baseband, processors, continuity?}}`；最多32项、整个文件256KiB，名称为1–128字节的非空文本。保存在本实例 `WEBSA_RECORDING_DIR/receiver-presets.json`，沿用目录独占锁；读取或存储格式校验失败时保留原文件并关闭能力，不重置为默认值。存储端校验十进制 u64 修订、重复名称及嵌套基带／处理器／连续性字段；旧文件省略的可选控制和未知模式名可以保留，实际设备与 provider 可用性仍由载入准入检查。

同一路径 POST 保存：`{type:"save", service_id, expected_revision, name, receiver_uid, receiver_revision}`；删除：`{type:"delete", service_id, expected_revision, name}`。修订均为十进制u64字符串。保存只从指定修订的已确认信道提取设置，并补齐实际生效的AGC／增益等默认值，不接收任意候选配置；同名替换。列表或信道修订冲突、旧服务代次返回409；非法输入400、未知项404、存储不可用503、写入失败500。成功在同步文件并原子替换后返回新列表；写盘不持有采集状态锁。客户端遇冲突显式刷新，不自动重试写入。

载入是浏览器将保存设置合入目标信道后走既有 `configure` 预检／显式确认：保留目标名称、颜色、标签和接收启停；不恢复UID、源时钟、任务或本地监听。预览后信道修订／服务变化会拒绝确认，要求重新预览。带外配置保留并等待采集覆盖，不可用模式仍由实际准入拒绝；预设不会移动共享采集或自动运行。

浏览器先校验预设响应的编辑结构，畸形设置不会进入预览。草稿保留 `continuity` 等没有表单输入的已预览设置；普通“应用设置”和切换信道后恢复草稿都保留这些字段，还原会清除它们。停收、颜色等独立设置仍取当前已确认状态。编辑器关闭、服务替换或 Preset 会取消其请求并使旧回调失效；已提交的服务器写入仍可能完成。

## 运行诊断

解码窗口和概览订阅已有接收机。已启用信道在没有浏览器／输出订阅时也启动采集和处理。Current 与保存信道即使同频也属于两个接收机，各自拥有外部解码进程。数字接收不受伴随音频静噪影响；模拟静噪状态只说明音频输出被关闭，不据此判断没有射频或协议信号。没有模拟监听链的模式报告 `squelch:null`。故障、恢复及积压优先于音频／协议等待。诊断和原始字段 JSON 面板提供复制按钮。

每路 `processing_statistics` 按逻辑 UID、服务身份、处理代次及流身份保存有界历史。`packets`、`searches` 分别报告已接纳、已开始、成功、失败、取消、拒收和在途；只有成功输入累计媒体秒。`occupancy` 包含排队、执行中、待发输出与搜索回传引用的字节／样本和单调年龄。`wall_ns`、可得的 `thread_cpu_ns` 与排队等待独立。水位描述源块；`service_consumed` 指服务已排出，不能当作远端扬声器已播放。`source_watermarks` 串联共享输入／PFB 观测和 fine 信道时钟，各观测不保证同时。模型请求／实际状态、静噪和协议窗口保留另行报告；不可观测的上游解码消费为 null。UTC 跳变、静噪关闭及协议窗口等待不等于源样本缺失。

浏览器轮询 `GET /api/sdr/receivers?detail=live`，只传送控制、时钟、故障、录制信息与精简的当前健康状态，省去 `processing_statistics` 和详细接收机观测。展开诊断面板后，立即请求 `GET /api/sdr/receivers?receiver_uid=<uid>`，并在可见时每两秒刷新；折叠、隐藏、切换身份或关闭窗口会取消请求与定时器。诊断响应不会替换实时控制状态。默认接口和命令回执仍保留完整诊断；当前处理状态只读取匹配 UID 与处理代次的数据。

周期 `STATUS` 消息及 `GET /api/state?detail=live` 省去详细线程统计、事件历史与保留的采集故障证据。`GET /api/state` 按请求生成完整诊断；普通频谱展示不会构造这些历史事件和计时 JSON。

频谱到达时独立更新轨迹累计和瀑布历史；数据显示合并为一个待绘制动画帧，按绘制耗时在 33–200 毫秒间调度，为媒体处理及浏览器栅格化留出时间。控件操作和触发完成仍同步刷新。

浏览器音频、解码入口各共用一个 Worker；每个接收机仍有独立 WebSocket、序列／样本时钟校验和有界队列，音频另有独立重采样器。控制命令和界面事件按逻辑源编号通过批量消息总线交接；界面消息在 40 ms 内合并，PCM 通过各自端口直接送入 AudioWorklet。停止一路只关闭该路订阅与连接，最后一路停止后释放 Worker；播放继续由 AudioWorklet 节点按 AudioContext 时钟驱动。电平表区域隔离样式、布局和绘制更新，由电平模型负责平滑，避免每路额外执行 CSS 裁剪动画。

AudioWorklet 按 50 ms 电平采样节奏合并电平、周期额度、状态和累计投递确认。确认只属于当前播放代次；重置会丢弃待发送的旧反馈，旧代次的确认不能释放新代次保留的 PCM。初始额度、播放状态切换和故障仍立即报告，音频样本、源连续性校验和播放时钟的语义保持不变。

`USB IQ capture handoff queue full: status=-130` 表示本服务采集适配层的有界 USB 交接队列溢出，采集连续性已丢失，恢复时报告 `source_gap`；这个错误码不能用来判断厂商 SDK 的带宽上限。

粗信道输出列按活动接收机频率生成，同一粗频点可由多路共享。局部调谐或删除在排空边界替换选择并释放闲置列，其他信道的连续流保持不变。没有粗信道消费者时，PFB 只维护输入历史与时钟，跳过 FIR／FFT 输出计算；独立频谱显示和原始 IQ 直接路由继续运行。

`sdr.actual.ddc_batch` 回报每个 PFB/fine 作业合并的物理 IQS 包数：每秒六千二百五十万个样本的硬件采集且没有原始 IQ 直接路由时为 4，其余为 1。每个物理包仍检查连续性，合批不改变输出时钟和帧格式。

PFB 输入／历史及输出存储在归还所有权后复用，普通收集也会及时取出完成作业，不必等到下一次准入达到上限。Linux 硬件采集通过 RTKit（`busctl`）为采集控制、采集拥有者、PFB 与 SDK／USB 线程申请公平调度器 nice -5。申请只在启动／重配边界执行；不可用或被拒绝时保留普通优先级并写入服务日志。Rust 显示、fine DSP 和接收线程使用普通优先级；SDK actor 共用 `san90-session` 名称，因此 SDK 选择也包含 DSP actor。这不预留 CPU，也不保证竞争负载下的连续接收。

`WEBSA_DIAGNOSTIC_SCHEDULING=1` 另用 Linux 调度／资源计数器对每 64 个 PFB 作业采样一次。`sdr.actual.pool_timing.scheduling_sample` 将 CPU、墙钟、可运行等待、上下文切换及缺页关联到同一作业／线程。正式 CPU 比较应关闭此采样。原生长复系数带通滤波器使用 f64 overlap-save，保留系数、因果延迟及每次调用的样本数；浏览器／WASM 构建保留直接卷积。

硬件采集只请求原始 IQ。默认 16,240 样本分析及可选 32,768／65,536 样本显示 FFT 在独立 SDK DSP 上下文中运行。每个线程最多保留一个执行中观测、一个最新待处理观测与一个最新完成帧；替换显示工作不会阻塞采集或丢弃 IQ。几何参数、校准比例、配置代次、源代次和样本连续性仍经过校验；默认分析继续提供射频功率和自动增益所需数据。`sdr.actual.display_fft.analysis` 报告默认分析线程状态；`dropped` 为替换的待处理观测数，`skipped_frames` 为替换的显示结果数。

`sdr.actual.fine_owners.channels` 按流身份报告独立 fine／直达执行线程的接纳、开始、成功、失败、取消、拒收数，含执行中引用的保留字节、排队等待、墙钟与可得线程 CPU 时间。`fine_faults` 报告失败源分支及最后完成／最早不可用输入边界。源分支失败会停止该身份的 IQ、处理及录制，其他路继续；获准的显式 `recover:true` 即使配置不变也创建新 IQ 身份；已记录中断时以 `reception.manual_recovery_allowed` 为准，设备或内部错误须先修复来源或配置。局部重配最多等待 100 毫秒，让目标路完成已接纳输入，期间收集各路就绪输出，完成后才关闭目标队列。忙碌拒绝保留原拥有者和可用队列，不产生源故障或创建替身。输入交接最多合并八块或四毫秒，控制／停止边界刷新不足批次；fine 输入限制为每路 1,024 排队块加一执行块。fine 和处理器的字节预算按实际输入率及样本格式计算两秒，限定为每路 4–64 MiB，包含执行中输入；处理器另有两秒媒体时长上限，两种输入队列均独立限制为 1,024 项。fine 诊断逐路报告 `input_bytes_limit`。fine 输出的三十二批／8 MiB 与发布交接的每身份三十二帧／8 MiB 是独立上限，均不等待下游腾空间。显示帧可替换，IQ 缺口明确失败源分支。fine／framer 汇总墙钟是各独立执行者之和，**不是**单线程占用率。
`processing_resources` 是准入时的几何诊断，策略为 `policy:"observed_runtime"`，不是 CPU 或模型内存预留预算。
当前 `host_resources` 观测和 `model_preparation` 排队/RSS 诊断分别报告。准备队首每次尝试获得名额时
重新观察内存余量和 CPU 并行度（最多等待 64 项、30 秒）。未知成本先允许一个准备任务；进程 RSS 增长可限制
并行度，但不能证明模型独占大小。提交再次校验配置修订；过期请求保留当前身份和传输 epoch。
排队票据在超时或资源观测异常展开时自动撤销。停止或身份失效会撤销旧结果的发布资格；排队期间不立即响应停止，已启动的原生模型调用由所属线程在返回后释放对象，不保证停止同步释放模型或进程 RSS 立即下降。cgroup v2 观测使用本进程对应的挂载层级及继承配额。

## 可执行定义与所有权

- [TypeScript 状态、命令和事件](../../../frontend/src/core/multichannel.ts) 是原型适配器边界。
- [Fixture adapter](../../../frontend/src/dev/fixtureReceiverAdapter.ts) 在内存中模拟服务，`transport=fixture`。
- [客户端会话](../../../frontend/src/ui/workbenchSession.ts) 保存选择、监听、desired/in-flight/confirmed。
- [IQ 不可变共享块与归档校验](../../../backend/src/iq_archive.rs) 保留实验 v1 兼容；生产归档使用独立 v2 模块。
- [语言无关 JSON Schema 与示例](../../../tests/fixtures/multichannel/draft-v1.schema.json) 供后端实现及兼容测试引用。

服务拥有采集、信道、处理、输出和记录。浏览器拥有窗口布局、固定/跟随绑定、选择及监听路由。
窗口管理器不向适配器发送停用或停止记录命令。本地音量、声像和 Solo 不在服务命令中。
Fixture 不启动声音、不写记录文件，刷新后任务重置；后端已支持持久 IQ 任务。

## 身份、时钟与状态快照

逻辑身份 `receiver_uid` 和 `source_id` 稳定，不以数组下标或 C1/C2 显示编号代替。
实际流以 `(source_id, epoch, channel_id)` 标识。独立 IQ 计划包含绝对频率、完整通带、过渡带和消费者最低率；
`processors=[]` 表示真正的仅 IQ。实际率由原生整数抽取计算，不能将 24414.0625 或 48840.130763116205 写成 24000/48000。

接收机配置／服务修订、流身份及精确时钟的整数和分数样本位置在线 JSON 中使用十进制字符串，包括超过 JS 安全整数范围的值。
这不是所有诊断字段的统一编码：例如 `source_watermarks.input_samples` 和 fine 线程计数仍为 JSON 整数，不能用它们代替精确源时钟；超过 JS 安全整数范围时，普通 `JSON.parse` 会失去精度。
`clock` 保留输入率、实际率、整数位置、分数位置、已知或未知延迟和 UTC 可靠性。
未关联可靠硬件时间时 `utc_reliable=false`。fixture 的位置/时钟只用于模拟，不能作为 RF 校验数据。

服务快照中 `receivers[]`、`outputs[]`、`recordings[]` 为动态集合；能力发布 `max_channels`。
当前原生运行上限为十六路；128 是长期容量目标，不表示当前可用路数。浏览器演示的容量单独配置。
记录有任务和独立轨道，轨道明确来源、格式、无损压缩策略、实际流片段及结束原因。
生产音频轨道来源为 `published_audio`，保留实际后端处理，排除浏览器本地监听增益与路由；旧实验 `analysis_audio` 标签不代表当前生产来源。

## 参数、预检与原子提交

`configure` 携带 `request_id`、信道 `expected_revision` 和完整候选配置。
`preflight` 返回接受/拒绝、故障、候选共享中心、受影响信道/记录轨道、资源估计。
草稿属于编辑视图、服务和接收机，显式预览后应用；切换信道保留各自草稿，还原、退役或服务重启清除对应草稿。
每路一个在途请求和最新待处理候选；创建、配置和退役共享有序客户端队列，因为原生准入按共享配置修订提交。重复退役请求合并，退役阻止该接收机后续配置、录制启动和输出启动意图。目标 UID 在提交时固定，切换选择不重定向请求。
拒绝后保留 confirmed 和实际流身份，显示错误与重试；迟到的旧修订快照不覆盖较新状态。
原生适配器包含传输超时、幂等 request_id 去重和服务重启代次；fixture 保持独立模拟。
服务替换或会话销毁取消排队命令、迟到回复、编辑和 UID 监听归属。退役后从主面板调谐，只有精确匹配的成功确认及健康接收机快照才恢复监听；拒绝、断线或超时取消该意图。新接收机使用新的音频默认值，已确认处理器保存实际增益、AGC、静噪和 NR 参数。

fixture 的 `scene` 携带整个信道集合、共享中心和服务 `expected_revision`。完整预检后一次更新；预检失败不修改任何配置。生产服务尚不开放原子场景提交。
场景仅载入配置，不自动重放正在记录任务或创建新的记录。基带变更分片；共享采集变化更新 epoch。
实际硬件在提交期失败应产生 `capture` 故障和真实边界，不能承诺物理回滚。
本地拒绝不是新的实际流边界。

## 动作、事件和错误示例

下列 `configure` 示例提交到 `POST /api/sdr/receivers`，其中 `service_id`、UID 和修订须替换为当前快照值。流边界和故障示例是适配器事件，不是 `/api/config` 命令：

```json
{"type":"configure","service_id":"service-current","request_id":"client-a-42","expected_revision":"7","receiver_uid":"rx-stable","config":{"name":"Unknown","color":"#76c9ed","tags":[],"enabled":true,"purpose":"iq","baseband":{"frequency_hz":7150000,"passband_hz":3000,"transition_hz":1000,"minimum_rate_hz":6000},"processors":[]}}
```

其他消息为 `create`、`retire`、`output`、`record_start`、`record_stop`、`scene`。
输出订阅、记录轨道和窗口互不推断启停。原生接收器退役会排空并结束其记录任务；历史仍可保留。
任务生命周期：`preparing → waiting/recording → finalizing → completed` 或 `failed`。
服务事件草案包括 `snapshot`、`stream_boundary`、`fault`；fixture 通过快照发布可观察状态，尚不单独发布边界事件。

```json
{"type":"stream_boundary","receiver_uid":"rx-stable","previous":{"source_id":"rf-1","epoch":"8","channel_id":"29"},"next":null,"reason":"gap","clock":null,"drained":false}
```

```json
{"scope":"recording","code":"DISK_FULL","message":"No space left on device","receiver_uid":"rx-stable","task_id":"task-7","retryable":true}
```

错误范围分别为 request、consumer、recording、receiver、receiver_iq、capture。慢订阅只失败自身；磁盘失败不停止健康接收；
采集故障影响所有依赖源。断流结束旧记录段，恢复必须有新流边界；完成或失败的记录不能由迟到计时器恢复为记录中。

## 原生信道接口与 IQ 订阅

生产 SDR 的 **信道工作台** 按钮为所有工作台窗口共享一个 `NativeReceiverAdapter` 和会话。
独立 Vite fixture 入口继续保留。原生界面按服务的模式描述符接入真实播放与解码结果，按能力开放后端 IQ 记录；模拟记录和场景仍不进入生产。

| 接口 | 行为 |
|---|---|
| `GET /api/sdr/receivers` | 服务确认快照、启动代次 `service_id`、能力、动态信道集合及已有样本的实际时钟 |
| `GET /api/sdr/receivers?detail=live` | 界面轮询快照，保留当前运行状态，省去按处理代次保存的诊断历史 |
| `POST /api/sdr/receivers/preflight` | 预检完整 create/configure/retire 候选，不提交 |
| `POST /api/sdr/receivers` | 提交 create/configure/retire；返回 `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | 单信道有序 IQBF/IQAX v2，带 JSON 起始与边界元数据 |

当前原生服务最多容纳十六个逻辑定义。启动期唯一 UID 显式映射到内部兼容槽；退役后复用槽产生新 UID，
仅停用定义则保留 UID。`processors=[]` 通过 `RatePlan::baseband` 实现真正仅 IQ，
不启动替身解调器或额外接收 worker。处理器按服务实际 provider 和模式可用性准入。

最近 128 个请求有有界幂等回执；同一 ID 的不同内容被拒绝。`configure.expected_revision` 是十进制字符串的信道修订。
适配器为请求 ID 加客户端命名空间并提交 `service_id`；旧启动代次不能向重启后的服务提交命令。
准入完成后、提交前再次检查修订。资源、RF 或滤波计划拒绝保留旧状态和身份；只改名称等元数据也推进修订。
物理采集失败按 capture 故障和非排空边界报告，不承诺任意 SDK 故障无缝回滚。

只有启用的 UID 能建立订阅。首个二进制帧前发送 `stream_start`，携带 `receiver_uid`、`source_id`、`stream`、
`format=cf32_le`、`baseband`、字符串 `first_sequence` 和 `clock`。时钟保留实际输入/输出位置、分数位置、
真实输入/输出率、以秒计的滤波延迟和 UTC 可靠性。原生整数 DDC 的分数为 0/1，不用名义整数率替代实际率；
迟加入的订阅取得实际首帧源位置。`stream_boundary` 包含前后身份、clock、reason 与 `drained`。
停止时旧尾部先于 `next=null` 和关闭；重调时旧尾部先于边界与新起始。
源故障发送 `drained=false`；慢订阅用 1013 关闭；无结束边界的意外断连标记消费者失败。
未受影响路保持身份和样本序列连续。

IQ 只编码一次，以不可变 `Bytes` 在网络消费者间共享。每个原生订阅限制 4 MiB 待消费字节、64 个 FIFO 项及
2 秒队列年龄/发送期限；超载只终止自身。混合解调的现有处理链仍解析已编码 IQ；迁移到有类型共享样本块属于后续工作，
本轮不构成 128 路容量验收。快照的 `outputs=[]` 由适配器补充本地真实订阅状态、已收样本/字节计数与起始确认。
关闭、Dock、Pop out、切换选择均不发送接收启停命令；关闭页面只关闭自身订阅。
进入 SDR 不再隐式停用辅助槽，重连不会重放浏览器旧接收集合。STATUS 发布 `sdr.receiver_api=multichannel-draft-v1`；浏览器首次加载即跟随服务实际模式，不重放保存的图形模式或 AM 检波设置。

`--fake` 发布 `capture.source_kind=fixture`：按各路真实输出率生成合成基带，时钟明确是该基带源，滤波延迟为零；
这条 fake 路径不经过 RF PFB。原生 Rust 测试另行将 CS16 样本送过 PFB/fine DDC，
在另一条路跨粗箱重调时逐样本比对未受影响路，并检查尾部、速率和源位置。
硬件实收及长期容量仍需独立测试窗口。

前端与原生构建后运行独立协议/浏览器门禁：

```sh
python tools/e2e/native_multichannel.py --artifacts /tmp/websa-p3-native
```

依赖 Playwright 和 websockets；脚本拥有并清理独立合成服务，不操作其他已有服务。

## 按信道监听与解码

`capabilities.mode_descriptors` 发布实际模式、provider、依赖可用性/原因、最低率、默认滤波带宽、伴随音频和结果类型。
原生工作台“信号设置”页可选择已有模拟/数字模式，并预览后提交；不可用依赖保留禁用选项和原因。
每路当前支持零个或一个主要处理器。可选 `bandwidth_hz`、`pcm_gain`（0..2）、`agc` 和 `squelch_dbm` 配置后端处理；
`pcm_gain` 会改变所有消费者收到的共享 PCM。协议输入继续绕过监听增强。

| 接口 | 行为 |
|---|---|
| `/ws/audio?receiver_uid=...` | 绑定一个已启用且有音频能力的逻辑信道，传输已有 AUD2；与 `channel` 参数互斥 |
| `/ws/decode?receiver_uid=...&ack=1` | 一个信道的有序 SDE2、已有图像快照及应用 ACK；与 `channel` 参数互斥 |

在采集健康时，按 UID 的音频／解码订阅在正常退役后以 `1000 / receiver retired` 关闭，
显式停用信道或移除处理器则以 `1000 / receiver stopped` 关闭。客户端清空播放缓冲和电平，等待目录更新或新的启用动作，
解码事件先交付已校验的有界 FIFO 尾部，再发布本地终态。
不把这两个明确终态当成故障或自动重连；其他关闭原因仍按原有传输／故障规则处理。

离开 SDR 时，IQ／音频／解码订阅以 `1000 / SDR inactive` 正常关闭。
按 UID 的客户端将其作为传输边界，可在返回 SDR 后恢复；无状态码关闭（1005）按共用传输重连策略处理。
明确退役、协议错误和有界服务故障重试仍分别分类。

SDE2 增加 `receiver_uid`、`service_id` 和 `baseband_stream`。逐帧归属按实际流映射确定，包括旧尾部，
不会因槽位复用把旧订阅指向新 UID。仅 IQ 请求音频或结果返回 409。AUD2/SDE2 的二进制布局和旧槽接口保持兼容。
独立基带的处理分支拥有自己的 `(epoch, channel_id)`；不能再把处理流身份当成 IQ 身份。
处理接入持续 IQ 时，以真实首块的输入位置创建处理起始；不回填此前未处理样本。源位置、分数率与已知/未知延迟仍保持原契约。

原生 PFB/DDC 比较实际源计划：频率、路由、完整通带、过渡带和真实抽取计划未变时，增删/切换处理器保留 IQ 身份与样本连续。
处理分支先消费排队旧 IQ，再按序排空重建；静噪状态按处理身份读取，健康 IQ 不因解码器恢复而重启。实际基带率/滤波改变继续发布旧尾部和新身份。
fake 中切换模式可能更换合成信号/回放文件及真实输入率，因此会形成真实 IQ 边界；它不证明 RF 连续性。

浏览器共享一个 AudioContext，每个实际监听 UID 仅一个现有 worker/worklet，使用本地 GainNode、StereoPannerNode，
汇入主增益和峰值压缩器。音量/声像采用 15 ms 渐变，取消 Solo 恢复手动监听组合；跟随选择只改变本地消费。
总静音保持音频消费，音频许可挂起时不订阅 PCM；主页面与 Pop out 内手势均可恢复。主面板按已确认的服务身份和 UID 订阅音频。
工作台获得本页面的扬声器所有权时，复用身份匹配的主面板 worker/worklet 和播放缓冲；身份不匹配的旧链路仍停止，避免重复播放或槽位复用后串台。
旧面板的音量仍是共享后端 PCM 增益，工作台“本地扬声器”控件才是当前用户增益。
原生音频订阅同样限制 4 MiB/64 项/2 秒；解码继续使用已有有界快照、队列与 ACK。

工作台固定/跟随 Decode 使用按 UID 的有界日志；FT8 表格、CW/外部文本、状态、外部 PNG 条带和原生 SSTV/传真 RGB/灰度行
复用结果渲染器。结果保留处理身份和源时钟，旧图像中断保留为 partial。原生 UID 日志每路最多 200 项/24 MiB，
单张原生图像最多 20 MiB，退役 UID 的浏览器缓存释放；旧兼容日志仍保留原有每槽预算和专用面板。
结果接收与窗口开关无关，新增窗口不会增加生产者。主页面关闭只结束本地消费。

UID 音频的临时传输断线采用有上限的退避重连；必须重新接收实际起始边界，清空旧缓冲，界面显示正在重连。关闭监听取消重试，接收器/协议/慢消费者错误不自动重试。退役和服务代次变化先移除消费所有者，再释放历史，拒绝迟到回调。直接点击跟随模式中的逐路监听会切换到手动混听，保留选中编辑对象与原试听路。

处理 worker 仍会从已编码 IQ 解析一次样本；本轮将处理身份/序号重映射作为类型明确的元数据传入，避免复制整份 IQ 或重编码。
上游 fine/成帧与处理消费者完全共享原生样本块仍未完成，不能据此宣称全链路零拷贝或长期容量验收。

```sh
python tools/e2e/native_multichannel_processing.py --artifacts /tmp/websa-p4-processing
python tools/e2e/native_multichannel_recovery.py --artifacts /tmp/websa-p4-recovery
```

门禁自行启动独立合成/回放服务，验证 UID 协议、实际浏览器输出 PCM 的增益/声像/总静音、多路混听、Solo/跟随、FT8 文本和 SSTV 像素。
它不构成实机 RF、扬声器听感、外部依赖全集或长期容量验收。

## 录制与实验边界

录制生命周期、当前音频格式、范围读取及 SigMF 导出以[录制参考](RECORDINGS.md)为准。
实验 WBIQ0001 仅通过 `iq-experiments` feature 编译，位于
`backend/src/iq_archive/v1.rs`；生产使用 WBIQ0002。Fixture 场景不表示生产原子场景能力。
