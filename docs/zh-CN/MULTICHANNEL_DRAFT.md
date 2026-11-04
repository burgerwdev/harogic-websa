# 多信道 P2–P5 状态与接口契约草案

采集快照将 `state:running|idle|failed` 与设备连接状态分开；`parameters_state:live|last_known|unknown` 分别表示当前源参数、保留的上次参数及源尚未打开。未知时带宽／输入率为 null，`center_hz` 仅为请求中心，epoch `"0"` 为哨兵，不表示活动流；独立基带夹具标记 `parameters_state:simulated`，共享显示窗口／率为模拟值，epoch `"0"` 为哨兵，真实基带输入以各路 clock 为准；硬件已知时输入率取 PFB 输入率，epoch 为 DSP 源代次，不能混用设备配置代次或下游 DDC 率。失败／停止保留原参数，重开源分配新 DSP epoch。启用硬件信道的变更须有健康活动采集。旧服务可能缺少 `parameters_state`，失败后仍报告旧版缺省值。

主面板输入中心频率后，将共享采集中心和 C1 监听频率一起设置到该点；已有放大框保持宽度并平滑滑到该点。IQ 带宽只修改采集带宽，保留逐路频率与模式；“移到中心”将采集移到 C1 当前频点。主面板其他调谐／解调／共享 PCM 参数仍作用于 C1；多路操作在工作台选中并编辑各 UID。采集变更可能中断所有信道并产生录制边界。


P6 首个切片为 `receiver_presets`：服务器持久保存单信道已确认的 `purpose/baseband/processors`，列表具独立u64修订。保存／删除不改接收计划；显式载入复用现有 `configure`，保留目标元数据及启停，不携带UID、活动任务或本地监听。过期预览不能覆盖后来配置。接口与限额见 [API](API.md)；持久接收定义与多路原子场景仍未交付。

`audio_archive_read` 为已结束发布音频任务及保持原身份的失败前缀增加只读样本帧选区和有界原生媒体试听。`audio_range_max_samples` 是u64字符串（`"1048576"`），`audio_range_max_seconds` 为60。输出保持原Float32位模式、实际率／声道／配置，原AUD2时钟条目加精确十进制裁切偏移，不绑定实时UID、不新增协议／分析来源或恢复录制。原任务校验、快照准备和传输共用既有单读名额。专用文件播放器／下载与实时监听独立。见[API](API.md)、[DEVELOPMENT](DEVELOPMENT.md)及 `native_multichannel_audio_playback.py`。

`iq_file_demodulate_offset` 为原生模拟文件处理增加选区内相位连续的正负频偏调谐，默认零。原录制频率及精确时钟留在来源SigMF中，派生清单单独保存所选频率与平移后的通带；两个端点须在原IQ和Nyquist内。浏览器仅在实际能力开放时显示控件，原限额与取消规则保持。见 [API](API.md) 及 `native_multichannel_tuning.py`。

`iq_file_demodulate` 能力开放只读有界模拟文件处理，`iq_file_demodulate_modes` 发布 AM／USB／LSB／NFM，时长上限60秒。原任务／服务／UID及逐块u64／分数时钟留在来源中；相对时间Float32音频为派生快照，不是恢复录制、协议输入或实时接收器。不分配／重绑硬件槽，原生DSP及响应共用单读名额和有界传输。播放器与下载tar来自同份结果，编辑／隐藏／销毁会取消并释放。见 [API](API.md) 及 `native_multichannel_demod.py`；数字文件解码继续后续交付。

`audio_flac_export` 为已结束的发布音频任务及原身份恢复前缀增加只读 FLAC 导出。各既有源片段按真实率／声道转换为 signed-24 PCM，固定比例舍入，明确统计饱和值。无损针对已量化的整数输入；原 Float32 位模式仍保存在 WBAU0001 与 WAV 导出中。文件附任务／UID／服务及未改动的逐帧来源。离线有界转换复用单下载名额，不创建实时接收器，不增加来源种类或自动续录；Opus 导出和文件管理现已另行提供；详情见 [API](API.md) 与 `native_multichannel_flac.py`。

P5 现支持 `kinds:["audio"]` 与 `audio_archive_export`，按描述符实际能力录制 AUD2 PCM，包含数字伴随音频与 codec 输出。`source:"published_audio"`／`format:"f32_le"` 与协议输入或分析音频分开；实际包配置、精确源时钟和样本位模式跨源／格式边界及前缀恢复保留。结束任务下载 Float32 WAV 片段与逐帧时间线，排除浏览器扬声器设置；后端任务共用既有三个 worker。协议输入录制与数字文件解码仍待后续，Opus 导出现已另行提供。详见 [API](API.md) 及 [DEVELOPMENT](DEVELOPMENT.md) 的独立 `native_multichannel_audio.py` 门禁。

P5 现已提供后端 `kinds:["results"]` 任务和 `results_archive_export`，消费实际 SDE2 发布支路；同 UID 可另启 IQ 任务。工作台按模式实际输出（含模拟模式遥测）决定能力，不按 `purpose` 排除。WBRS0001 恢复、准确增量图像及校验 tar／JSONL 下载语义见 [API](API.md)。浏览器消息／PNG 导出仍是有界会话快照；协议输入音频和数字文件解码尚未实现。

状态：draft-v1，包含 P2 原型/实验、原生 P3 接收/IQ 和 P4 监听/结果及 P5 后端 IQ 记录切片，服务契约仍在演进。最初产品基线为
`3978ed2439f0bf9385414122e3e720b0a63f2b99`。设计依据是仓库外三份多信道方案。

## 可执行定义与所有权

- [TypeScript 状态、命令和事件](../../frontend/src/core/multichannel.ts) 是原型适配器边界。
- [Fixture adapter](../../frontend/src/dev/fixtureReceiverAdapter.ts) 在内存中模拟服务，`transport=fixture`。
- [客户端会话](../../frontend/src/ui/workbenchSession.ts) 保存选择、监听、desired/in-flight/confirmed。
- [IQ 不可变共享块与实验容器](../../backend/src/iq_archive.rs) 由可选 `iq-experiments` 特性提供。
- [语言无关 JSON Schema 与示例](../../tests/fixtures/multichannel/draft-v1.schema.json) 供后端实现及兼容测试引用。

服务拥有采集、信道、处理、输出和记录。浏览器拥有窗口布局、固定/跟随绑定、选择及监听路由。
窗口管理器不向适配器发送停用或停止记录命令。本地音量、声像和 Solo 不在服务命令中。
Fixture 不启动声音、不写记录文件，刷新后任务重置；后端持久 IQ 任务已接入 P5 切片。

## 身份、时钟与状态快照

逻辑身份 `receiver_uid` 和 `source_id` 稳定，不以数组下标或 C1/C2 显示编号代替。
实际流以 `(source_id, epoch, channel_id)` 标识。独立 IQ 计划包含绝对频率、完整通带、过渡带和消费者最低率；
`processors=[]` 表示真正的仅 IQ。实际率由原生整数抽取计算，不能将 24414.0625 或 48840.130763116205 写成 24000/48000。

所有 u64、配置/服务修订和样本位置在线 JSON 中使用十进制字符串，包括超过 JS 安全整数范围的值。
`clock` 保留输入率、实际率、整数位置、分数位置、已知或未知延迟和 UTC 可靠性。
未关联可靠硬件时间时 `utc_reliable=false`。fixture 的位置/时钟只用于模拟，不能作为 RF 校验数据。

服务快照中 `receivers[]`、`outputs[]`、`recordings[]` 为动态集合；能力发布 `max_channels`。
原型运行上限默认三路，类型与适配器可测试更多路数；128 是容量目标。
记录有任务和独立轨道，轨道明确来源、格式、无损压缩策略、实际流片段及结束原因。
监听处理与文件内容独立，音频轨道来源当前草案为 `analysis_audio`。

## 参数、预检与原子提交

`configure` 携带 `request_id`、信道 `expected_revision` 和完整候选配置。
`preflight` 返回接受/拒绝、故障、候选共享中心、受影响信道/记录轨道、资源估计。
草稿只属于编辑视图，显式预览后应用；切换目标/取消撤销未提交草稿。
每路一个在途请求和最新待处理候选；目标 UID 在提交时固定，切换选择不重定向请求。
拒绝后保留 confirmed 和实际流身份，显示错误与重试；迟到的旧修订快照不覆盖较新状态。
原生 P3 已加入传输超时、幂等 request_id 去重和服务重启代次；fixture 保持独立模拟。

`scene` 携带整个信道集合、共享中心和服务 `expected_revision`。完整预检后一次更新；预检失败不修改任何配置。
场景仅载入配置，不自动重放正在记录任务或创建新的记录。基带变更分片；共享采集变化更新 epoch。
实际硬件在提交期失败应产生 `capture` 故障和真实边界，不能承诺物理回滚；该路径待 P3/P6。
本地拒绝不是新的实际流边界。

## 动作、事件和错误示例

以下动作只是适配器消息，不是对现有 `/api/config` 接受命令的声明：

```json
{"type":"configure","request_id":"client-a-42","expected_revision":"7","receiver_uid":"rx-stable","config":{"name":"Unknown","color":"#76c9ed","tags":[],"enabled":true,"purpose":"iq","baseband":{"frequency_hz":7150000,"passband_hz":3000,"transition_hz":1000,"minimum_rate_hz":6000},"processors":[]}}
```

其他消息为 `create`、`retire`、`output`、`record_start`、`record_stop`、`scene`。
输出订阅、记录轨道和窗口互不推断启停。原生接收器退役会排空并结束其记录任务；历史仍可保留。
任务生命周期：`preparing → waiting/recording → finalizing → completed` 或 `failed`。
服务事件草案包括 `snapshot`、`stream_boundary`、`fault`；fixture 通过快照发布可观察状态，尚不单独发布边界事件。

```json
{"type":"stream_boundary","receiver_uid":"rx-stable","previous":{"source_id":"rf-1","epoch":"8","channel_id":"29"},"next":null,"reason":"gap","clock":null}
```

```json
{"scope":"recording","code":"DISK_FULL","message":"No space left on device","receiver_uid":"rx-stable","task_id":"task-7","retryable":true}
```

错误范围分别为 request、consumer、recording、receiver、capture。慢订阅只失败自身；磁盘失败不停止健康接收；
采集故障影响所有依赖源。断流结束旧记录段，恢复必须有新流边界；完成或失败的记录不能由迟到计时器恢复为记录中。

## 原生 P3 信道接口与 IQ 订阅

生产 SDR 的 **信道工作台** 按钮为所有工作台窗口共享一个 `NativeReceiverAdapter` 和会话。
独立 Vite fixture 入口继续保留。原生界面按服务的模式描述符接入真实播放与解码结果，按能力开放后端 IQ 记录；模拟记录和场景仍不进入生产。

| 接口 | 行为 |
|---|---|
| `GET /api/sdr/receivers` | 服务确认快照、启动代次 `service_id`、能力、动态信道集合及已有样本的实际时钟 |
| `POST /api/sdr/receivers/preflight` | 预检完整 create/configure/retire 候选，不提交 |
| `POST /api/sdr/receivers` | 提交 create/configure/retire；返回 `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | 单信道有序 IQBF/IQAX v2，带 JSON 起始与边界元数据 |

当前原生服务最多容纳三个逻辑定义。启动期唯一 UID 显式映射到内部兼容槽；退役后复用槽产生新 UID，
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

## P4 按信道监听与已有解码

`capabilities.mode_descriptors` 发布实际模式、provider、依赖可用性/原因、最低率、默认滤波带宽、伴随音频和结果类型。
原生工作台“信号设置”页可选择已有模拟/数字模式，并预览后提交；不可用依赖保留禁用选项和原因。
每路当前支持零个或一个主要处理器。可选 `bandwidth_hz`、`pcm_gain`（0..2）、`agc` 和 `squelch_dbm` 配置后端处理；
`pcm_gain` 会改变所有消费者收到的共享 PCM。协议输入继续绕过监听增强。

| 接口 | 行为 |
|---|---|
| `/ws/audio?receiver_uid=...` | 绑定一个已启用且有音频能力的逻辑信道，传输已有 AUD2；与 `channel` 参数互斥 |
| `/ws/decode?receiver_uid=...&ack=1` | 一个信道的有序 SDE2、已有图像快照及应用 ACK；与 `channel` 参数互斥 |

SDE2 增加 `receiver_uid`、`service_id` 和 `baseband_stream`。逐帧归属按实际流映射确定，包括旧尾部，
不会因槽位复用把旧订阅指向新 UID。仅 IQ 请求音频或结果返回 409。AUD2/SDE2 的二进制布局和旧槽接口保持兼容。
独立基带的处理分支拥有自己的 `(epoch, channel_id)`；不能再把处理流身份当成 IQ 身份。
处理接入持续 IQ 时，以真实首块的输入位置创建处理起始；不回填此前未处理样本。源位置、分数率与已知/未知延迟仍保持原契约。

原生 PFB/DDC 比较实际源计划：频率、路由、完整通带、过渡带和真实抽取计划未变时，增删/切换处理器保留 IQ 身份与样本连续。
处理分支先消费排队旧 IQ，再按序排空重建；静噪状态按处理身份读取，健康 IQ 不因解码器恢复而重启。实际基带率/滤波改变继续发布旧尾部和新身份。
fake 中切换模式可能更换合成信号/回放文件及真实输入率，因此会形成真实 IQ 边界；它不证明 RF 连续性。

浏览器共享一个 AudioContext，每个实际监听 UID 仅一个现有 worker/worklet，使用本地 GainNode、StereoPannerNode，
汇入主增益和峰值压缩器。音量/声像采用 15 ms 渐变，取消 Solo 恢复手动监听组合；跟随选择只改变本地消费。
总静音保持音频消费，音频许可挂起时不订阅 PCM；主页面与 Pop out 内手势均可恢复。工作台获得本页面的扬声器所有权，
暂停旧主面板播放链，避免重复播放。旧面板的音量仍是共享后端 PCM 增益，工作台“本地扬声器”控件才是当前用户增益。
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

## P2 压缩实验约束

实验格式 WBIQ0001：文件头、多个 BLK1（有界 JSON 元数据与原始/Zstd 数据）、最终 IDX1 索引。
每块最大 1 MiB，候选时长 0.25 秒；Zstd 等级 1–3，只有更小才保存压缩块。
每块保存轨道/逻辑/实际流身份、位置、分数时钟、真实率、样本数、修订、编码、字节数、位置与校验。
FNV-1a64 仅检测损坏，不作认证；P5 定稿校验与容器版本。
停止消费 writer 后写最终索引并 flush，实验入口另行 sync_all。
扫描器返回完整块和 `complete=false` 来表示缺少索引或不完整尾部；元数据/校验损坏明确报错。
随机定位验证索引元数据和块校验，不把压缩字节当普通 SigMF 样本导出。

扫描器和实验报告为离线测试保留全部解码块，RSS 是整个实验进程累计高水位。真实 FT8 fixture 仅带实际采样率，归档位置使用 fixture 相对位置加上超过 2^53 的测试原点，并非恢复出的原始 RF 时间戳。
以上 v1 实验不包含实时 worker 或产品回放；正式 IQ 记录使用下面的 v2，v2 波形回放见下文。

工作台和按 UID 的 Decode 在状态更新时保留原节点，包括松键激活的按钮、草稿、选区、滚动和展开状态。
Decode 将消息批次合并为可排序/筛选表格，提供 UTC、图像历史，并复用实时画布/PNG 节点。
AM/WFM/DRM 及解码进度直接展示真实读数；原生状态按事件类型保留，监听 NR 状态不会覆盖解码遥测。
“清空视图”经确认隐藏此视图已有消息和图像，实时状态和其他视图的共享有界历史继续保留；不改变接收或后端记录。
时间默认采用浏览器接收时间，只有结果显式提供协议时隙 UTC 时才使用它；原生 FT8 保留实际上报的时间偏移。
源位置仍在事件元数据中，不据此推断绝对 RF UTC。

产品界面将处理方式、本地扬声器、后端录制和输出消费者作为独立选择。
“信号设置”只有一个模式选择，包括不创建处理器；内部 `purpose` 仅为 API 兼容而派生。
移除独立“处理／结果”页签及重复模式／移除处理器按钮，已有保存页签迁移到“信号设置”。
“实时输出”列出按 UID 的实际 IQ／音频／结果 WebSocket 地址；本页 IQ 传输检查折叠，并明确不保存文件。
按钮用途提示区分选中编辑、独立设置、信号／结果查看、本地监听与后端录制。

音频窗口不显示文本筛选和清空操作。AM/WFM 与旧 Decode 共用 `audioScope.ts`；
CW 在增量文本之外独立保留 `cw-level` 实时快照；DRM 展示电台、节目和锁定读数。
消息 CSV／JSONL 下载按当前模式／文本筛选导出全部保留分页，属于会话快照。
JSONL 保留服务／UID、流身份和精确源时序，浏览器接收时间与显式协议 UTC 分开。
CSV 转义文本并避免公式解释，数值测量仍保持数值。PNG 导出选中的保留图像，
未完成／部分图像的文件名包含 partial，保留缺失行的中性灰带，RGBA 内存限制为 20 MiB。
这些导出不发送接收／录制命令，不构成后端音频／结果持续录制或 IQ 归档下载／回放／SigMF。

## P5 后端 IQ 记录

服务发布 `record_start`/`record_stop` 和 `recording_kinds:["iq","results"]` 后，工作台可直接启动和停止后端记录。任务从 preparing、recording、finalizing 到 completed/failed；一个任务当前含一种 IQ 基带或已发布结果轨道。多实例目录独占，任务保存原 receiver_uid、service_id、track_id 和实际 epoch/channel_id。换频/变率建立新段，正常停用接收器结束任务；断流/采集错误失败并保留校验前缀。视图和监听变化不提交记录配置。重启只恢复历史并标记 SERVICE_INTERRUPTED。

启动成功回复之前，worker 同步首卷、初始索引和任务清单，随后才开放样本接入。API 在释放采集状态锁后等待最多 2 秒；相同请求的并发重试共享准备结果和同一回执。确认后立即终止服务也能恢复准备中任务。

正式容器 **WBIQ0002** 与实验 **WBIQ0001** 区分，二者都不是 SigMF。每卷 `.wbiq` 从 8 字节 magic 开始，记录前缀为 4 字节 kind、u32le JSON 长度、u32le payload 长度、u32le CRC32。CRC32/IEEE 覆盖前缀前 12 字节、JSON 原字节和 payload。BLK2 的 JSON 保存 meta、codec、raw_crc32，payload 为逐位 CF32 LE 或独立 Zstd level 1 帧；压缩不变小时回退 raw。raw_crc32 校验解压字节。meta 包含原服务、段、帧序号、频率/基带及 IqBlockMeta（实际率、原始位置、输出位置、分数、延迟、修订和 u64 十进制字符串）。原生整数抽取的分数为 0/1，未知 UTC 标为不可靠。

每 128 块写一个 IDX2 索引页，内存只保留一页；JSONL sidecar 逐块同步。EVT2 保存段边界。END2 写块/样本总数和前面各记录 CRC32 小端值序列的 CRC32，索引核对后才能报告完整。256 MiB 或 4096 块滚卷，卷边界不建立新源段。元数据上限 256 KiB、块上限 1 MiB；每任务至多 4096 段，超限明确失败。停止封闭接入、排空已接纳块、写索引/尾部并 fsync，然后原子替换并同步 task.json。

恢复逐块验证，不累计解压数据；重建 JSONL 索引。最后一条记录截断只恢复此前完整块，CRC/元数据损坏明确失败，原文件保留且不跳过坏块。队列最多 4 MiB/64 项/2 秒（含正在写入的块），最多三个活动 worker；积压失败会报告已接纳/已保存字节，不能宣称未写入的样本已完成。磁盘或压缩错误不停止接收和健康网络消费者。已结束 IQ 任务现支持下述有界波形回放与样本范围 SigMF 导出；协议输入音频及数字文件解码仍属后续工作。


已结束任务通过 `/api/sdr/recordings/{task_id}/range` 读取（见 [API](API.md)）。每请求选择一个片段内 1–1,048,576 个复数样本，最多 4096 个校验块及 4 MiB 来源元数据。sidecar 用于定位，v2 在成功返回前复核完整元数据、压缩记录 CRC 和裸样本校验。缺口、重叠、身份／率／频率／基带变化或越界明确失败。可跨卷，不修改原始文件或接收器。一次只准入一个阻塞读取／下载，名额保持到响应传输结束；全部浏览器窗口共用一个波形缓冲。

`.sigmf` 为 POSIX tar，内含 `recording/recording.sigmf-data`（裸交错 CF32 LE）与 `recording/recording.sigmf-meta`（SigMF 1.2.6）。`core:sample_rate` 为真实分数采样率，`core:frequency` 为片段频率。每次导出将样本下标归零，不伪造 `core:datetime`。可选 `websa` 扩展版本 1.0.0 在 Global 保存任务／服务／UID、源片段、所选起点／长度、原任务故障及基带配置。各 Annotation 的 `core:sample_start`／`core:sample_count` 是导出数据内的相对范围；`websa:clock` 保存完整原始 `IqBlockMeta`，`websa:skip_samples` 为块内十进制裁切量，另保留原序号及配置修订。因此部分块也保留精确 u64／分数／延迟来源，不经 JavaScript 四舍五入源位置，未知绝对 UTC 继续明确。不同片段／采样率保持独立 Recording，可由集合端点一起下载。

独立读取器门禁：`python tools/e2e/native_multichannel_archive.py --artifacts /tmp/websa-iq-archive --sigmf-python /path/to/sigmf-venv/bin/python`。在独立环境安装官方 `sigmf==1.6.1`（schema 1.2.6）；Python／浏览器工具仍仅用于开发。门禁核对真实网络／归档／范围字节、官方 SigMF 解析、实际波形／定位、420px 下载／Dock 及重启后的历史。波形回放查看已录复数样本，不运行实时接收 DSP 或生成音频。


流式 `/export` 端点与 `iq_archive_export` 能力补充有界 `/range` 读取，最多 1,073,741,823 个复数样本，各 tar 成员受 POSIX ustar 上限约束。它复用校验访问器和读取准入，准备时不累计全部数据／注释，随后流式输出并复核数据与来源。两项 64 KiB 队列积压两秒明确失败；销毁响应取消生产者，以终止性写入错误避免重试空转。原 `/range` 限额、精确 u64／分数来源保持。界面以浏览器原生下载保存所选范围或整段，只有波形预览拥有 8 MiB 缓冲；不同流片段保持独立 Recording，也可一起打包为集合。

### 跨片段 SigMF 集合

`/collection` 将同一已结束任务的全部非空已校验片段导出，至多 4096 片段、合计至多 1,073,741,823 样本。界面依据 `iq_archive_collection` 开放操作，服务发布并强制执行集合限额。根目录各元数据／数据对保留各自真实率、频率及原始时钟；`recording.sigmf-collection` 按顺序保存基名和元数据 SHA-512。全部片段在成功前校验，共用流式导出的名额、取消和有界传输；仅逐片段计划（前缀／ID 至多 2 MiB）和集合清单（至多 2 MiB）累积。后段损坏会拒绝预检或结束不完整传输，不用健康子集冒充整个任务。失败任务仅开放恢复后的已校验前缀，保留原 UID／服务／故障。不执行 DSP 文件回放、跨片段样本拼接或绝对时间对齐。规范见 [SigMF Collection](https://sigmf.org/#sigmf-collection-format)，独立读取器与上游 Schema 差异见 [DEVELOPMENT](DEVELOPMENT.md)。
