# 录制参考

[文档目录](../INDEX.md) · [接收机接口](RECEIVERS.md) · [协议](API.md)

当前音频任务直接保存 WAV／FLAC／Opus 文件。末节仅说明既有 Float32 日志的读取兼容，不是当前写入格式。

## 任务生命周期与恢复

能力发布 `max_channels:16` 与 `max_recording_tasks:16`。`recording_resources` 返回 `active_tasks`、`max_tasks`、`available_tasks`、`queue_bytes_per_task`、`queue_items_per_task` 和 `max_queue_age_ms`。准备及收尾中的任务保留名额，回收后释放。复数样本、发布音频和结果共享全服务十六个名额；同一路同时录三种内容占三个名额。信道定义数量与录制任务数量分别计限。

IQ 记录通过同一 receiver API 接入：`record_start`（`receiver_uid`、`kinds:["iq"]`）和 `record_stop`（`task_id`），带 `request_id` 和当前 `service_id`。无需接收器配置预检或浏览器 IQ socket。服务实际开放后才发布 `recording:true`、`recording_kinds:["iq","results","audio"]` 和对应 commands；存储不可用时不发布操作，并提供 `recording_unavailable_reason`。快照 `recordings[]` 包含原服务身份、轨道、状态、已保存字节和错误，刷新/重连只读取此实际状态。`GET /api/sdr/recordings/{task_id}/index?volume=iq-000000.jsonl&offset=0` 返回最多 128 个已持久块索引及十进制 `next_offset`，不是样本下载或 SigMF。

启动时，已完成的 IQ、音频和结果录制直接加载持久化清单，只检查卷名、文件大小及 IQ 索引是否存在，不逐块解压或重写索引。已有索引按需读取，内容校验在范围读取和导出成功前执行。异常中断／失败任务，以及文件缺失或元数据不一致的已完成任务才执行恢复。索引查询要求任务已结束（活动任务返回 409，删除后返回 404），与删除共用归档读取锁，并返回 `Cache-Control: no-store`。删除从当前历史中移除任务，成功前同步隐藏删除标记；启动时忽略尚未清理的删除标记目录。

IQ、音频及结果 worker 将已排队的包合并同步，最多等待50毫秒，每批至多32项／约1 MiB（单个更大的有界输入可独占一批）。数据、存在时的IQ旁路索引和清单同步后才发布该批计数。原4 MiB／64项／2秒准入预算包含所有排队和在途包，合批不重置最旧输入的期限。原块、源时钟、片段边界和文件格式保持；停止排空并同步最后一批。

带持久记录能力的服务另发布 `state_revision`（服务代次内单调递增的十进制 u64），与接收配置 `revision` 分开。浏览器拒绝迟到的旧状态快照，记录进度和最终状态不因配置修订未变化而回退。

恢复后的 `raw_bytes` 只计已校验保留的来源数据；`accepted_bytes` 至少覆盖该前缀，并保留可能包含未完成尾段的较大持久化计数。`queued_bytes` 清零。不能用已接纳字节数推断可下载的样本数。

## 音频格式与文件管理

音频格式与文件管理：`record_start` 对 `kinds:["audio"]` 接受可选 `audio_format:"wav"|"flac"|"opus"`，默认 **Opus**。新任务使用 `tracks[].container:"websa-audio-files-v1"`；录制线程直接写入所选格式，不再另存原始 PCM 日志。WAV 保存 Float32 PCM，FLAC 按下文固定比例量化为 signed-24 PCM，Ogg Opus 使用 48 kHz 有损 VBR（单声道 48 kb/s、立体声 96 kb/s）。Opus 录制需要含 `libopus` 的 FFmpeg。来源或配置变化时创建新文件，连续来源也会约每 60 秒完成一个文件，最多 4096 个片段。停止时排空并完成编码器收尾，再将任务标为完成。持久保存文件大小、SHA-256、来源样本数及逐帧时间线。异常中断只恢复已完成文件的前缀，排除当前未完成片段。

对于新任务，`GET /api/sdr/recordings/{task_id}/audio` 下载包含 `task.json`、`frames.jsonl` 和原样已存音频文件的 tar。`/audio/flac` 和 `/audio/opus` 只接受任务实际录制格式，不匹配返回 422。下载只校验摘要和复制文件，不调用音频编码器。旧 `websa-audio-v1` Float32 日志继续支持原有 WAV／FLAC／Opus 导出。仅旧日志的 Opus 导出使用私有临时目录（编码文件上限 2 GiB、来源时间线上限 64 MiB），完成、取消或失败后清理。所有路径共用归档读取名额。

工作台主悬浮窗底部提供持久化的默认音频录制格式下拉菜单。首次使用和成功执行 Preset 后为 Opus；用户明确选择的 WAV 或 FLAC 会在刷新后恢复。活动录制保持开始时的格式，新录制完成后只显示实际录制格式的下载。成功执行 Preset 还会清除工作台模式记忆及未应用草稿，恢复瀑布图映射和峰值列表数量；失败或无关回执不重置偏好。

频率默认显示和初始输入草稿四舍五入到 1 Hz（MHz 保留六位小数）；未编辑频率时，修改其他设置或切换单位仍保留原始精确调谐值。显式输入支持小于 1 Hz 的精度，失焦、提交和单位切换不会截断小数；显式输入整数则按该整数调谐。录制历史只在录制页显示，按四舍五入到 Hz 的当前频点匹配任一已保存片段，支持旧服务身份。文件库支持名称、已存信道名及带单位的频率搜索（`14M`、`14.2 MHz`、`14200k`），带单位数字按频率前缀匹配。新任务将 `receiver_name` 与可编辑文件名分开保存。旧任务使用已有名称和频点关联，未保存的原信道名无法还原。

`record_rename`（`task_id`、`name`）和 `record_delete`（`task_id`）通过现有 `/api/sdr/receivers` 命令端点提交，保留 `request_id`／服务身份语义。只允许管理已完成任务和已排空的失败任务。名称去除首尾空白，允许 1–128 字符、拒绝控制字符；重命名仅修改持久元数据，删除移除整项录制。下载占用单读名额时拒绝操作，文件系统操作不持有采集状态锁；命令重试保持幂等。删除先将目录移至隐藏墓碑，清理中断后不会重新恢复任务。工作台“录制文件”统一展示当前和历史信道，支持搜索、类型／状态筛选、试听／导出、行内重命名与明确确认删除。复选框支持多选及全选当前筛选列表中已结束的录制；批量删除前显示数量与合计大小，逐项提交，失败项保留选中便于重试。每项、全部及所选文件的大小按十进制 B／KB／MB／GB／TB 自动换算，最多一位小数。

`record_start` 现接受 `kinds:["audio"]`，按模式描述符的实际 `audio` 输出准入，包含 FT8 伴随音频与 DRM/DRM+ codec 音频，不以 `purpose` 排除。录制来源固定为已发布 AUD2 Float32 PCM（`source:"published_audio"`、`format:"f32_le"`），包含实际后端共享增益／AGC／NR／静噪或 codec 配置；浏览器音量／声像／静音／Solo 不进入文件。它不是协议输入音频；新 Opus 任务在录制期间编码，下载原样返回已存文件。每任务一种轨道，同 UID 的 IQ、结果、音频三个任务占全服务十六个名额中的三个，各任务保留 4 MiB／64项／2秒预算。无音频帧的 codec 正常 waiting，不按 IQ 五秒超时失败。准备确认先同步清单与首卷；停止排空，处理器移除正常结束，处理错误隔离健康 IQ。

## 已存音频范围读取

`audio_archive_read:true`、`audio_range_max_samples:"1048576"` 和 `audio_range_max_seconds:60` 开放 `GET /api/sdr/recordings/{task_id}/audio/range?segment=audio-000000&start=0&count=20000`。起点／长度为单音频片段内无符号十进制**样本帧**（立体声一帧包含两个声道），使用受检 u64 运算。仅读结束任务或已收尾的失败校验前缀。每次1–1,048,576帧，且按原8000–192000 Hz整数率至多60秒，保持原单／双声道。未知参数／非法计数400，活动／收尾409，未知404，片段／范围／数据错误422，共享读取名额占用429。

有界 `audio-range.tar` 含 `selection.json`、`frames.jsonl` 和 `audio.wav`（含fact的IEEE Float32 LE）。旧版日志和直接录制的 WAV 保持实际发布音频原位模式；FLAC／Opus 试听解码已保存文件，Opus 重采样至来源采样率并可能补齐尾部。清单明确标注量化、有损及重采样。原共享处理保持，本次读取不额外调整增益或归一化。清单保存原任务／服务／UID／故障、片段相对起点／长度、精确首音频位置、实际格式及PCM／时间线SHA-256。所选时间线条目保留完整原AUD2头及源时钟，额外 `selection` 用十进制保存 `record_skip_frames`、`frames` 和选区从零计数的 `range_sample_start`。部分帧不推算／改写源位置及分数，不编造绝对UTC；原生播放器时间从所选首音频样本开始。

`recording_audio_read.rs` 对新录制在成功前校验全部文件和时间线摘要；对旧录制流式校验**整个**既有日志及片段摘要。新录制的 WAV 试听保持原始 Float32；FLAC／Opus 通过 FFmpeg 解码已保存文件，Opus 试听重采样至来源采样率，必要时补齐重采样尾部。选区清单明确标注量化、有损及重采样。当前音频无随机读取索引，准备耗时随完整任务长度增加。内存保留一个校验条目、至多8 MiB所选PCM加58字节WAV头、4096条／4 MiB时间线和2 MiB清单。之后不可变快照复用单读名额的准备／producer／body寿命、两项64 KiB／2秒传输背压；取消待两方退出后释放。原记录文件不变。浏览器按需加载一个独立原生媒体播放器，分配前校验16 MiB封装上限，逐传输块检查长度；编辑／隐藏／换任务／释放时取消并回收旧播放。Pop out／Dock移动同一媒体节点，下载保存同份已载入快照。实时监听与文件播放独立，旧服务隐藏入口。这是有界已存音频回放；连续长文件播放、直接播放 Opus 文件及数字文件解码仍单独推进；Opus 导出使用上文专用端点。

## IQ 范围读取与 SigMF 导出

浏览器小范围读取在分配前要求合法的 `Content-Length`，并逐块核对实际接收长度。
CF32 波形必须恰为选定样本数乘 8 字节，小范围 SigMF 归档最多 16 MiB；缺失、超量或不一致的长度会取消 reader 并拒绝结果。
已存音频及文件解调分别使用 16 MiB／20 MiB 封装上限，沿用同一有界读取逻辑。整段／集合原生下载不进入波形缓冲区。

服务通过 `iq_archive_read:true` 和 `iq_range_max_samples:"1048576"` 发布已结束任务的 IQ 读取能力。`GET /api/sdr/recordings/{task_id}/range?segment=<segment_id>&start=0&count=4096&format=sigmf` 选择一个片段内从零计数的样本范围；`start`/`count` 为十进制整数，每次 1–1,048,576 个样本（8 MiB CF32）、最多 4096 块和 4 MiB 来源元数据。`format=cf32` 返回校验后的裸样本，`format=meta` 返回 SigMF 元数据，默认 `sigmf` 返回 POSIX tar `.sigmf`，包含同名 `.sigmf-data`／`.sigmf-meta` 文件。成功前重新解压并校验全部所选块，可跨卷但不拼接不同片段／采样率，响应 no-store。活动／收尾中任务返回 409，未知任务 404，非法参数 400，越界／缺失／损坏数据 422，并发读取 429。单个阻塞读取 worker 在采集状态锁外执行 I/O 和解压，读取名额保持到响应传输结束，中止响应会释放名额。失败恢复任务只开放已校验前缀，元数据保留原故障、服务及 UID。

流式导出通过 `iq_archive_export:true` 和十进制 `iq_export_max_samples:"1073741823"` 发布。`GET /api/sdr/recordings/{task_id}/export?segment=<segment_id>&start=0&count=4194304` 将已结束任务的所选范围下载为标准 SigMF。CF32 数据成员小于 8 GiB（POSIX ustar 大小上限），元数据成员具有相同的单文件上限；现有 `/range` 波形／小范围读取限额保持。HTTP 成功前完整校验所选范围，随后在输出数据与注释时重新校验；只保留一个校验块和两项 64 KiB 传输队列。源文件在导出中变化会使响应失败，不会完成另一份样本文件。读取名额覆盖准备、生产者和响应寿命，关闭响应会取消工作并释放名额。传输队列积压两秒结束该导出生产者，接收与记录继续。浏览器使用原生下载，不将大文件读入 Blob。不同片段仍保持独立 Recording，可通过 `/collection` 一起下载。

`GET /api/sdr/recordings/{task_id}/collection` 将同一已结束 IQ 任务的全部非空已校验片段下载为一个 POSIX tar `.sigmf` 归档，不接受查询参数。能力 `iq_archive_collection:true` 发布十进制字符串 `iq_collection_max_samples:"1073741823"`（任务所有片段合计）和整数 `iq_collection_max_segments:4096`。各片段在归档根目录分别成为从零计数的 `segment-000000.sigmf-data`／`.sigmf-meta`，保留实际率、频率、精确时钟及原任务／服务／UID／故障。根目录 `recording.sigmf-collection` 的有序 `core:streams` 对象保存生成的文件基名与元数据文件实际字节的 SHA-512。不暗示跨片段连续、重采样或绝对时间对齐。空任务、超限、任一片段缺口／损坏在成功响应前整体拒绝为 422；活动／收尾 409、未知任务 404、查询参数 400、读取名额占用 429。集合与 `/range`、`/export` 共用单读名额、取消和两个 64 KiB 传输项；预检保留有界逐片段计划（前缀／ID 至多 2 MiB，加至多 2 MiB 集合清单），样本与注释流式输出。预检后来源改变会结束不完整响应。工作台“全部片段”使用原生下载，波形缓冲维持原限额。

## 离线模拟解调

`iq_file_demodulate:true`、`iq_file_demodulate_modes:["am","usb","lsb","nfm"]` 和 `iq_file_demodulate_max_seconds:60` 开放 `GET /api/sdr/recordings/{task_id}/demodulate?segment=<id>&start=0&count=20000&mode=am&if_bw=1500`。仅读已结束 IQ 任务或收尾后的失败校验前缀。范围是单片段内十进制整数，最多 1,048,576 复数样本／60 秒／4096 块／4 MiB 来源；实际 IQ 率须为 1000–192000 S/s。`if_bw` 为 50–20000 Hz（USB／LSB 为原生载波保护区保留至少101 Hz），且不大于实际 IQ 率的 0.45；AM/NFM 完整对称带宽或 USB/LSB 单边带必须落在原已录对称通带内。未知参数／模式或非法计数 400，来源／滤波／数据错误 422，活动 409，未知 404，共享读取名额占用 429。

响应为有界 POSIX tar `iq-demod.tar`，含 `render.json`、`source.sigmf-meta` 和 `audio.wav`。原生 `AnalogDemod::new` 以实际 IQ 率执行 IF／音频 FIR 及有状态线性重采样，输出 48000 Hz 单声道 IEEE Float32 WAV（含 fact），固定单位增益，关闭 AGC／降噪／静噪，沿用原生模式去加重。这是新生成的离线音频，不是此前发布 PCM。各请求在选区起点从零建立因果滤波器，保留起始暂态，只保存实际输出；不补预滚／尾部零样本，不拼接片段。音频样本下标／48000 为相对播放时间，解调总延迟明确未知；原 u64／分数／延迟及块内裁切偏移在来源元数据中原样保留，不编造 UTC。SHA-256 覆盖所选 CF32、来源元数据和生成 PCM。来源元数据描述原 IQ，本 tar 不附 IQ 数据成员；原文件和身份不变。

处理在采集锁外，不分配实时接收槽。限一个校验源块、每轮8192样本原生 DSP、最多11,520,008 PCM字节（另有有界WAV构造）、4 MiB来源和2 MiB清单。与IQ／结果／音频共用单读名额、准备／producer／body寿命、取消及两项64 KiB／2秒传输预算。成功前完整验证并生成派生快照，输出读内存，源之后改变不会改变该快照。浏览器仅保留一个有界tar／播放器（20 MiB封装上限），替换请求取消，修改范围／隐藏／换任务释放音频，下载同份音频及来源。旧服务隐藏文件DSP。数字文件解码与连续长文件播放继续单列。

新增 `iq_file_demodulate_offset:true` 时，文件解调接口另支持可选有限数 `offset_hz`（默认 `0`）。正值选择原录制 IQ 中心以上的频率。共享原生 NCO 在 IF FIR 前执行 `IQ * exp(-j*2*pi*offset_hz*n/actual_iq_rate_hz)`；选区首样本相位为零，跨校验块／卷连续。所有非零频偏（含小于1Hz）均实际应用，零值绕过混频并保持原解调结果。平移后的两个通带端点必须同时落在原对称录制通带与实际 IQ Nyquist 内，不作频率回绕；USB／LSB沿用原生非对称边带。`render.json.processing.tuning` 保存频偏、原中心、所选频率、原输入坐标下的频带和相位规则。`source.sigmf-meta` 及其 CF32 摘要仍描述原中心下未改动的 IQ。不改变样本时间、不补预滚或新增延迟估计。旧服务无该能力时，浏览器隐藏频偏控件并省略参数。非有限／非法频偏400，不可用频带或非法所选频率422。

## 解码结果归档

`record_start` 也接受 `kinds:["results"]`，要求健康、启用且描述符实际发布文字、表格、图像或状态的处理器。当前每任务一种轨道；同一 UID 可用两个任务同时录 IQ 与结果，共享全服务十六个任务名额。`results_archive_export:true` 开放 `GET /api/sdr/recordings/{task_id}/results`，不接受查询参数，流式下载 POSIX tar `results.tar`，内含 `task.json` 与 `events.jsonl`。每行包含 `version:1`、主机接收时刻 `received_at_ms`（十进制毫秒字符串）、原基带 `source` 信息及完整 `envelope`，保留 SDE2 原处理序号／时钟、服务／UID 和 `baseband_stream`。源样本位置及分数保持十进制字符串，接收时刻不建立样本绝对 UTC。清单保留故障、前缀信息及“只记录启动后实际发布事件”策略。图像保留原始增量 RGB／Gray8／base64 PNG 载荷和完整性元数据，缺少前部行仍是 partial；后端不重建独立 PNG，也不补录浏览器历史。

结果日志内部格式为 WBRS0001：各 JSON 记录独立 Zstd level 1 压缩，原始块回退，16 字节类型／原始大小／存储大小／CRC32 报头，每卷有计数／字节／摘要校验尾部。每记录最多 3 MiB，共用 4 MiB／64 项／2 秒队列，包含在途工作；压缩、同步、恢复和下载均在采集锁外。无消息时正常等待；移除处理器排空并结束结果轨道，处理失败只使结果录制失败，健康 IQ 继续。结果按处理流身份分段，旧异步事件仍归原段。异常中断后重启只恢复已校验前缀，不续录或重绑。下载先预检全部保留记录及尾部，再边传输边校验，后续改变导致不完整响应；与 IQ 共用单读名额及两项 64 KiB／2 秒传输队列。查询参数 400、未知 404、活动／收尾 409、种类或数据错误 422、并发 429。结束的空结果任务可下载空 JSONL。协议输入音频与数字文件解码仍未开放；Opus 导出及模拟文件重新解调见上文专用端点。

## 旧版 Float32 日志兼容

内部 WBAU0001 保存每帧实际配置 JSON 和原始 AUD2 头／PCM；使用共同的独立 Zstd1/raw／CRC32 日志 codec，每卷 256 MiB 或4096帧滚动、至多4096卷／片段，配置 JSON 每帧16 KiB、片段清单2 MiB。处理流／音频格式起始、源配置或 RIFF 大小边界各自新段；不拼接缺样、不填静音。原处理帧携带配置快照，排队旧帧不套用当前设置。异常中断后重启恢复已校验前缀，保留原 UID／服务／故障，不自动续录。

`audio_archive_export:true` 开放 `GET /api/sdr/recordings/{task_id}/audio`，无查询参数，流式下载 `audio.tar`。包内为 `task.json`、逐帧来源 `frames.jsonl` 和各非空片段的 IEEE Float32 LE WAV（含 fact 样本数）；样本位模式不变，无重采样／量化。每 WAV 小于4 GiB，长期连续源在此上限前分段。JSONL 包含原 AUD2 96字节头的十六进制、精确源位置／分数／延迟／实际率／音频位置和配置，不能据此宣称绝对 UTC 或跨段对齐。完整预检、输出 SHA-512 复核、单读名额、producer/body寿命和两项64 KiB／2秒背压与 IQ／结果下载共用；改变产生不完整响应。400／404／409／422／429沿用，空结束任务可下载空时间线。WAV依据 [Microsoft RIFF/WAVE 规范](https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/WAVE/Docs/riffmci.pdf)。

对旧 Float32 日志，`audio_flac_export:true` 开放 `GET /api/sdr/recordings/{task_id}/audio/flac`，不接受查询参数，仅读取已结束音频任务及失败任务的校验前缀。原生下载 `audio-flac-s24.tar` 包含 `task.json`、未改动的逐帧来源 `frames.jsonl`，以及每个非空源片段独立的 `.flac`。来源仍是包含共享后端处理、排除浏览器扬声器设置的实际发布 PCM。每个有限 Float32 声道值转换为 `q = clamp(round_away_from_zero(x * 8388608), -8388608, 8388607)` 的有符号 24 位整数。不加抖动、不归一化、不调增益、不重采样或填补缺样。清单声明量化规则，并以十进制字符串保存逐段 `clipped_values`（舍入后超出整数范围的交错声道值个数）、样本数和编码长度。正 1.0 饱和，负 -1.0 可准确表示。FLAC 无损针对选定的整数 PCM，不保持原 Float32 位模式；原始日志及 Float32 WAV 接口继续可用。真实率、声道、配置及片段边界保持，不暗示协议输入来源或绝对 UTC。

`recording_flac.rs` 使用纯 Rust `flacenc` 0.5.1，关闭并行特性，采用固定预测器／Rice 编码、4096 样本固定块及不补齐的短尾帧。STREAMINFO 按 [RFC 9639](https://www.rfc-editor.org/rfc/rfc9639.html#name-streaminfo) 保存准确样本数及打包 signed-24 LE PCM 的 MD5。内存只保留一个校验日志条目、一个编码帧和有界摘要；清单／计划合计至多2 MiB、至多4096片段。预检编码至 sink 以计算固定 tar 长度、STREAMINFO 和量化统计，输出重新编码并在 tar 尾部前核对来源及编码帧 SHA-512；来源改变产生不完整响应。不写中间音频文件。与 WAV／结果／IQ 共用单读名额、producer/body 取消寿命及两项64 KiB／2秒传输，沿用400／404／409／422／429；结束的空任务导出元数据及空时间线。
