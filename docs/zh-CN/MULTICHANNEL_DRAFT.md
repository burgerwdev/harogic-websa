# 多信道 P2/P3 状态与接口契约草案

状态：draft-v1，包含 P2 原型/实验及原生 P3 接收/IQ 切片，服务契约仍在演进。最初产品基线为
`3978ed2439f0bf9385414122e3e720b0a63f2b99`。设计依据是仓库外三份多信道方案。

## 可执行定义与所有权

- [TypeScript 状态、命令和事件](../../frontend/src/core/multichannel.ts) 是原型适配器边界。
- [Fixture adapter](../../frontend/src/dev/fixtureReceiverAdapter.ts) 在内存中模拟服务，`transport=fixture`。
- [客户端会话](../../frontend/src/ui/workbenchSession.ts) 保存选择、监听、desired/in-flight/confirmed。
- [IQ 不可变共享块与实验容器](../../backend/src/iq_archive.rs) 由可选 `iq-experiments` 特性提供。
- [语言无关 JSON Schema 与示例](../../tests/fixtures/multichannel/draft-v1.schema.json) 供后端实现及兼容测试引用。

服务拥有采集、信道、处理、输出和记录。浏览器拥有窗口布局、固定/跟随绑定、选择及监听路由。
窗口管理器不向适配器发送停用或停止记录命令。本地音量、声像和 Solo 不在服务命令中。
Fixture 不启动声音、不写记录文件，刷新后任务重置；后端持久任务留到 P5。

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
输出订阅、记录轨道和窗口互不推断启停。先停止任务才能删除活动定义；历史仍可保留。
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
独立 Vite fixture 入口继续保留。原生界面按真实能力禁用模拟记录、场景、监听混音和生成的 Decode 结果；
真实播放与结果集成属于 P4，持久记录属于 P5。

| 接口 | 行为 |
|---|---|
| `GET /api/sdr/receivers` | 服务确认快照、启动代次 `service_id`、能力、动态信道集合及已有样本的实际时钟 |
| `POST /api/sdr/receivers/preflight` | 预检完整 create/configure/retire 候选，不提交 |
| `POST /api/sdr/receivers` | 提交 create/configure/retire；返回 `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | 单信道有序 IQBF/IQAX v2，带 JSON 起始与边界元数据 |

当前原生服务最多容纳三个逻辑定义。启动期唯一 UID 显式映射到内部兼容槽；退役后复用槽产生新 UID，
仅停用定义则保留 UID。`processors=[]` 通过 `RatePlan::baseband` 实现真正仅 IQ，
不启动替身解调器或额外接收 worker。兼容层可保留已有处理模式，其完整工作台播放/结果集成留到 P4。

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

## P2 压缩实验约束

实验格式 WBIQ0001：文件头、多个 BLK1（有界 JSON 元数据与原始/Zstd 数据）、最终 IDX1 索引。
每块最大 1 MiB，候选时长 0.25 秒；Zstd 等级 1–3，只有更小才保存压缩块。
每块保存轨道/逻辑/实际流身份、位置、分数时钟、真实率、样本数、修订、编码、字节数、位置与校验。
FNV-1a64 仅检测损坏，不作认证；P5 定稿校验与容器版本。
停止消费 writer 后写最终索引并 flush，实验入口另行 sync_all。
扫描器返回完整块和 `complete=false` 来表示缺少索引或不完整尾部；元数据/校验损坏明确报错。
随机定位验证索引元数据和块校验，不把压缩字节当普通 SigMF 样本导出。

扫描器和实验报告为离线测试保留全部解码块，RSS 是整个实验进程累计高水位。真实 FT8 fixture 仅带实际采样率，归档位置使用 fixture 相对位置加上超过 2^53 的测试原点，并非恢复出的原始 RF 时间戳。
目前没有有界记录 worker、队列积压、分卷、崩溃持久索引恢复或产品端回放；这些是 P5 的入口。
