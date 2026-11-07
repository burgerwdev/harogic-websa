# 架构

[文档目录](INDEX.md) · [状态归属](reference/STATE.md)

## 系统边界

Rust 服务拥有设备、采集、信道化、接收处理和持久录制；C++ 适配层串行访问专有 SDK。
TypeScript 浏览器负责控制、可视化、解码结果和本地音频播放。Python 提供离线数值参考、
样本与台架工具；保留的 WASM 产物属于可选诊断路径。

```text
浏览器命令 → 准入／模式所有者 → 常驻 SDK 会话
SDK IQ → 有界采集队列 → PFB → 独立信道 fine DDC → 不可变 IQ 帧
                     └──── 直达宽带 DDC ──────┘
IQ 帧 → 独立接收处理线程 → 发布 PCM／解码结果
      → 录制线程         → 有界 HTTP／WebSocket 消费者
SDK 迹线 → 频谱／密度／瀑布渲染
发布 PCM → 浏览器缓冲／重采样 → 本地增益／路由 → 音频输出
```

SWP、RTA、SDR、谐波和相噪模式共享一个物理设备。Server 模式释放设备交给 SoapySDRServer，
进入模式与启动服务器是独立动作。请求先验证再替换活动状态；硬件实际值与请求值分开保存。
[状态归属](reference/STATE.md)定义转换规则。

## 采集与信号处理

### 阶段映射

路径检查验证以下实际实现文件。

| 阶段 | 模块 |
|---|---|
| SDK 会话与采集 | `backend/sdk/san90_sdk.cpp`, `backend/src/bin/websad/service_capture.rs` |
| 路由、命令提交与生命周期 | `backend/src/bin/websad/service_routes.rs`, `backend/src/bin/websad/service_commands.rs`, `backend/src/bin/websad/service_lifecycle.rs` |
| 设备状态、RTA 提交与准入 | `backend/src/bin/websad.rs`, `backend/src/bin/websad/rta_control.rs`, `backend/src/bin/websad/service_preflight.rs` |
| 流水线编排 | `backend/src/bin/websad/sdr_pipeline.rs`, `backend/src/bin/websad/service_ddc.rs` |
| 粗 PFB | `backend/src/pfb.rs`, `backend/src/pfb_pool.rs` |
| fine DDC 与采样率 | `backend/src/fine.rs`, `backend/src/narrow.rs`, `backend/src/rate_plan.rs` |
| 接收处理线程 | `backend/src/bin/websad/receiver_workers.rs`, `backend/src/receiver.rs` |
| 模拟／数字内核 | `dsp/core/src/analog/`, `dsp/core/src/digital/`, `dsp/core/src/pipeline.rs` |
| 降噪 | `backend/src/nr.rs`, `backend/src/nr_neural.rs`, `dsp/deepfilter/src/` |
| 传输／订阅 | `backend/src/wire.rs`, `backend/src/bin/websad/service_stream.rs`, `backend/src/bin/websad/subscriber_stream.rs` |
| 录制 | `backend/src/bin/websad/recording.rs`, `backend/src/bin/websad/recording_audio_files.rs` |
| 浏览器接收状态 | `frontend/src/core/multichannel.ts`, `frontend/src/ui/workbenchSession.ts` |
| 播放 | `frontend/src/audio/sdrAudio.ts`, `frontend/src/audio/sdrAudioWorker.ts`, `frontend/src/audio/sdrAudioWorklet.js` |
| 参考内核 | `tools/reference/`, `tools/fixtures/`, `tests/fixtures/` |

生产 SAN shim 在复制前校验有限采样率、固定 16,240 复数样本的 CS16 包，以及格式、字节数和指针。
对应假 SDK 契约使用厂商头文件编译实际 shim，不执行物理采集。Soapy 是独立适配路径：
传输 MTU 固定为 16,384，SDK 包最多 1,048,576 样本，分配前将主机环形缓冲与转换暂存总量限制为 64 MiB。
Soapy 还在遍历前拒绝超过 SDK 64 项 Ethernet 数组的设备数量；参考电平在停止流或转为整数前须为有限值且在 `int` 范围内。
Python 参考采集独立限制 CS16 包为 4 MiB，并在 `ctypes.string_at` 前核对长度与格式；
它的无 SDK 测试不构成 Rust 采集验证。

生产 RTA shim 在分配前校验帧尺寸及有限频率范围，迹线、位图和有限捕获累积三个数组合计最多 64 MiB，
包括保留容量和替换时的峰值；打开、重配置与回滚共用检查。Rust SWP／RTA／PNM 另行验证适配层返回的尺寸，
SWP／RTA 帧最多 65,536 点并拒绝非有限数值读回。这些是指定缓冲区的边界，不是整个 SDK 或服务 RSS 上限。

### PFB、fine DDC 与时钟

在 62.5 MSps 下，50 MHz 可用采集使用二倍过采样 PFB：128 bin、hop 64，bin 间隔
488.28125 kHz，各粗信道流为 976.5625 kSps。这不代表支持 128 个产品信道；产品容量来自
`contracts/service-capacity.json`。窄采集采用实际几何，宽带 provider 可走直达 IQ 路径。

有序 PFB 线程共享不可变计划，各信道独立拥有振荡器和可变滤波历史。fine DDC 完成剩余频移、
滤波及采样率转换。准入校验整个通带及过渡带，而非只检查中心频率。
实际小数采样率必须在接口和文件往返中保留。

PFB 使用四个工作线程；执行中、等待顺序和待领取结果合计最多六项。线程完成后即可接收下一作业，
每份作业携带完整历史与绝对输入位置，输出仍按原序交付。达到保留上限且已有可交付结果时，
提交返回背压，由调用方先领取结果再重试；不能等待接收结果来释放仍未领取的容量。

IQ 分配不可变、由消费者共享，仅在网络／文件需要时编码。源身份和精确样本位置区分采集 epoch、
基带流及处理代次。接收机修订、流身份和精确时钟中的 u64 使用十进制字符串；诊断计数及 RTA 触发元数据仍有 JSON 整数，不能一概按字符串解析。间隙创建明确边界，不补造样本或绝对 UTC。
准确布局和诊断见[接收机契约](reference/RECEIVERS.md)及[二进制协议](reference/API.md)。

## 并发与故障归属

采集、PFB、fine DDC、接收处理、录制和订阅边界均使用有界队列。共享采集失败影响所有依赖信道；
接收处理或消费者故障在所属边界内隔离，不得静默丢弃 IQ 后沿用旧时钟。显示帧可以替换。
显式重试按需要建立新身份。

PFB 拥有者的致命控制错误及 Rust panic 会明确结束输出队列。控制回包先交付已收集尾帧，再发布终止原因；轮询、终态诊断和停止保留同一原因。panic 后只读取已发布快照，关闭队列后析构原拥有者，不继续处理异常后的 DDC。此边界不捕获原生 abort 或段错误。

fine 线程保持各路顺序。高耗时推理和外部解码不在采集锁内执行。跨线程墙钟时间之和不是单线程
CPU 占用。准入限制约束结构，算法可用不代表任意组合都能在当前主机持续运行。
单元或合成测试通过不能证明真实射频长时性能。

HTTP 与 WebSocket 共用命令提交所有者，统一发布状态、身份和流退役边界；过期预检拒绝不推进传输 epoch。
生命周期模块先绑定监听端口，再启动所有者；清理前协作等待发布器结束。
发布器自身异常退出、它所等待的采集／准备／GNSS 任务 panic，或意外正常返回，会关闭 HTTP 监听并保留原始错误，避免继续返回过期健康状态。
循环的 unwind 边界保留在途模型准备句柄，先通知停止接入，再等待任务完成，最后释放 SDK 和录制所有者；普通单路接收错误仍在所属分支隔离。
SWP／RTA／PNM 控制请求将排队与回复纳入同一期限，传输超时后不再接受新请求。
四种 SDK 模式退役最多等待 250 ms，未返回的所有者继续占用会话排他门，直至原线程完成销毁；不能从其他线程并发关闭或重开。
这不保证厂商调用或进程静态析构能结束，永不返回的 SDK 调用仍需启动器的停止期限兜底。

RTA 普通配置和触发配置共用提交路径：SDK 接受后更新实际值；设备断开立即退役所有者、清除捕获完成状态并按冷却期限重试。
相噪先验证完整请求的频偏窗口，再更新下一周期配置；拒绝不修改正在运行或待启动的周期。

模型准备在队首每次尝试启动时重新观察主机/cgroup，并以获准时的 RSS 为基线。进程 RSS 增长仅调节准备并行度，
不代表模型独占内存或接收准入保证。

可选脉冲消隐器最多修复连续 `window` 个高幅度样本，跨块保留预算；更长的信号透传并更新逐样本 RMS 估计。
修复使用已知干净端点或保持前值，不增加样本或延迟。首块初始化和可用前视仍依赖分块。

原生 CW 所有者接收 8–192 kHz 单声道 PCM，每次最多向桥接提交 100 ms。
ggmorse 的输入暂存按实际重采样请求在回调前扩容，分析窗口和样本时间不随缓冲尺寸改变。
本地补丁及固定上游出处记录于 `tools/vendor/ggmorse/UPSTREAM`。

可选诊断解调 ABI 在构造前验证有限速率，输出率为 8–192 kHz，并保留有效分数采样率。
每次处理先按现有重采样状态计算输出长度，最多 1,048,576 个 PCM 或适配后的复数样本；
容量不足时不消费输入，立体声差分块也保留供足量缓冲区重试。这是诊断 ABI 的约束，不能代替原生接收机准入。

录制线程在采集锁外编码、压缩、校验和同步。IQ、结果和音频共享十六个任务名额。
停止先关闭准入再排空。启动按原任务身份恢复完整数据或校验前缀，不恢复已退役接收机。
新音频任务直接写媒体文件，旧 Float32 日志保留读取兼容。归档操作共享有界读取名额。
详见[录制参考](reference/RECORDINGS.md)。

## 浏览器状态与音频

一个原生 receiver adapter 和会话拥有工作台的服务器状态。窗口拥有几何、选择和编辑草稿，
不各自创建物理接收机。请求身份、预期修订及服务代次阻止迟到回复覆盖更新状态。

`workbenchApp.ts` 管理窗口协调和表单；`workbenchChannelStrip.ts` 拥有折叠条草稿、读数、滚动文本和监听。
手势延迟切换视图期间，动作和播放电平仍绑定实际显示的 service/UID；卸载清理监听、定时器并使待完成回调失效。
会话串行队列在实际派发创建命令时确定已有 UID，连续创建各自返回明确身份；多个新增项无法归因时显式报错。
Preset、服务替换和销毁使旧监听预览失效；取消编辑同时重置聚焦表单的 DOM，避免旧草稿重新进入缓存。

后端音频处理影响已发布 PCM 和录制；浏览器增益、声像、静音与独听只影响本地监听。
RF IQ 功率、解调 PCM 电平和本地输出电平是不同测量，显示必须说明来源。
浏览器缓冲和重采样意味着接收时间不是播放时间。解码历史及图片具有明确保留预算。

渲染位于状态及帧契约下游。设备／显示参数归属集中管理；新增控制应复用 intent／acknowledgement
流程，避免乐观写入已确认状态。依赖和注册检查辅助发现结构退化。

## 维护重点

主要风险是职责广泛的服务／前端协调模块、重复 UI 投影、旧归档兼容和与生产路径并列的实验工具。
当行为变更提供可测试边界时，按所有权和稳定接口拆分；避免只移动文件却保留隐式耦合。
合并测试文件时保留独立数值参考、时钟／身份测试和资源边界回归。

[开发指南](DEVELOPMENT.md)规定验证方式和维护优先级；[运行与排障](OPERATIONS.md)说明当前能力边界。
第三方源码和许可证保留在对应组件目录，许可入口见[依赖与许可](DEVELOPMENT.md#依赖与许可)。
