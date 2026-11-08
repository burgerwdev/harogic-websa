# 开发指南

[文档目录](INDEX.md) · [架构](ARCHITECTURE.md)

## 源码构建

要求 Linux、Rust ≥1.95、C++17 编译器、pkg-config 和 FDK AAC ≥2.0.3。
前端使用已提交的 npm lockfile，以及测试栈支持的 Node（22.22+、24.15+ 或 ≥26）。
基础 Python 工具要求 ≥3.10；完整开发环境和下述锁定依赖安装使用 Python ≥3.12（CI 为 3.12／3.13）。
测试和浏览器台架使用较新的异步接口，不能把基础工具的最低版本当作测试环境要求。
硬件另需专有 SDK；Soapy 和外部解码器可选。参见本页[依赖与许可](#依赖与许可)。

```bash
make native                     # release Rust 服务，单构建任务
make frontend                   # 类型检查及 Vite 构建，不构建 WASM
make sdk-shim SDK_ROOT=/opt/htraapi
./run.sh                        # 或 ./run.sh --fake
```

重构建分别执行，不使用无限制 `make -j`。以下 Rust 命令采用一个构建任务和一个测试线程，
前端 Vitest 限两个 worker。Python 工具使用虚拟环境：

```bash
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements-dev.txt -c requirements-lock.txt
python -m playwright install chromium
```

`make wasm` 仅重建可选诊断产物。`make soapy-driver` 将固定版本、应用补丁的驱动构建至用户目录。
provider／模型安装见[工具指南](../../tools/README.md)。

## 按职责测试

| 命令 | 验证职责 |
|---|---|
| `./test.sh` | 原生服务契约、Python 工具、Ruff 和前端行为 |
| `make wasm-test` | 原生共享 DSP 内核测试，不运行浏览器 WASM |
| `make ci` | 上述测试、样本／版本／文档／架构检查、Soapy 补丁及前端构建 |
| `make e2e-fake` | 独立模拟服务：浏览器 smoke、状态回归及视口几何 |
| `make hw-test` | SAN 硬件、tinySA 和浏览器测量检查 |
| `make bench` | 将运行中服务与已有性能基线比较 |

托管工作流是 `.github/workflows/ci.yml`，另运行专项工作台／录制浏览器检查。
本地 `make ci` 不表示所有托管或硬件任务都已执行。
小内存主机串行执行重构建、解码器／降噪测试和性能测量。确认内存余量后，独立轻量浏览器测试可有限并行；
真实 RF 持续测量独占运行，避免测试负载影响结果。

针对单个原生回归：

```bash
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib TEST_NAME -- --test-threads=1
```

即使输入相似，只要职责不同就应保留：数值正确性、序列化、源时钟、状态转换、消费者取消和真实
浏览器接线分别捕获不同故障。删除测试前先明确替代覆盖。能够验证可观察行为时，避免源码字符串
断言。硬件测试和合成 probe 保持独立标签与前置条件。

浏览器异步条件须等待实际解析结果，并验证持续为假的条件会超时；不能把 Promise 对象本身当成成功。
依赖额外采集输入的手工用例应显式标为跳过，显式运行但缺少输入时须报错，不能直接返回并计作通过。

## 扩展实现

控制遵循[状态归属](reference/STATE.md)，传输改动遵循[接口](reference/API.md)。后端能力是权威来源，
前端不硬编码限制。容量源位于 `contracts/service-capacity.json`，版本源位于 `pyproject.toml`。

新增模式需要目录描述、准入／采样率约束、处理所有权、输出／时钟行为和失败路径。
复用共享 DSP 内核，避免并行维护浏览器版本。UI 控制需要复用 intent／receipt 流程、提供双语文本、
可访问焦点行为，以及对应边界的回归测试。

注释解释单位、归属、不变量、数值推导和不直观的取舍。删除过时迁移叙述及只是重复代码的注释。
保留第三方源码出处和许可证。文档双语在同一变更中同步。

源码审查分别记录人工阅读、全库静态检查和行为回归的范围；文件清单或结构扫描不能代替逐段阅读。
故障注入先证明原实现失败，再验证修复后的拒绝、释放和恢复；Python 参考、Soapy 和生产 SDK shim 的结论分别说明。

## 样本与验证证据

`tests/fixtures/` 的 golden 帧和 DSP 样本连接 Python 参考生成器、Rust 内核和 TypeScript 解码器。
生成器的 `--check` 是 CI 契约。主动重新生成并审查数值变化，不能仅为使测试通过而更新样本。

十六信道合成验证可使用现有 probe，指定未使用的持久输出目录：
`python tools/bench/eight_channel_bench.py --channels 16 --seconds 30 --warmup 5 --artifacts <new-directory>`。
历史文件名不限制信道数量；合成检查不能证明 RF 接收。

SAN90 与 PlutoSDR 已建立可控 RF 耦合时，使用匹配当前源码的 SDK shim，关闭占用设备的服务后运行：

```bash
python tools/bench/multichannel_rf.py --shim <matching-sdk-shim.so> \
  --channels 16 --seconds 600 --gain -10 --artifacts <new-external-directory>
```

该工具独占启动服务和发射源，检查十六路 AM／NFM／USB／LSB 的 32 条 IQ／音频流、精确样本连续性、
身份及首尾已知音调。采集为 62.5 MSps／50 MHz，激励位于 434.85–435.15 MHz；
因此不能由此推断全 50 MHz 的射频质量或所有解码器组合。发射增益需适合实际耦合，示例值不保证任意台架通过。
退出会停止自有服务、恢复 Pluto 设置，并在保存摘要后删除音调 PCM。此类测量期间暂停重构建和浏览器测试。

硬件／性能证据必须记录二进制修订或摘要、设备／固件、采集几何、主机负载、时长、预期信号、
连续性及产物。不要调整无关进程来制造性能通过。将报告保存在 CI 产物或仓库外的测试目录，及时删除本次产生的大型临时采集。
性能对比工具只将本机回环 HTTP 监听者的 CPU 计入该实例；远程服务或无法唯一确认的监听者没有本地 CPU 测量值。

## 发布与清理

构建服务后重启目标实例，并检查 `./status.sh` 和进程实际可执行文件。成功重建后，旧进程可能仍持有
旧 inode。如果暂不重启，交付说明必须明确提醒。

`make package-runtime` 复制现有服务／前端／适配器，不执行编译，须先构建匹配产物。
打包助手按支持范围包含原生降噪依赖及声明；外部 provider 仍独立安装。
验证实际打包启动及动态库要求，不能以开发机正常代替可移植性验证。

`make clean` 删除缓存／dist、保留依赖及 Rust target；`make clean-all` 还删除依赖和 target，
保留已提交诊断 WASM。不能顺手删除用户录制、排障所需日志或无关缓存。
文档只维护当前使用说明、接口、构建流程和维护约束。更新时同时修改双语，运行文档链接及结构检查，
删除已被替代的页面。单次运行的截图、日志和测量报告放到 CI 产物、发布附件或仓库外的测试目录；
需要长期回归的数据应提炼为有来源说明的 `tests/fixtures/` 样本。旧文档与历史报告通过 Git 查阅。

## 依赖与许可

- 有权授权的自有代码：**AGPL-3.0-only，附 HAROGIC SDK／FDK AAC 第 7 条例外**（`LicenseRef-Harogic-WebSA`）。参见[范围](../../LICENSING.md)、[第三方声明](../../THIRD_PARTY_NOTICES.md)及[源码打包／未决权利](LICENSING.md)。此前 MIT 授权继续有效。
- 原生监听降噪包含 DeepFilterNet3，以及 GTCRN、UL_UNAS、FastEnhancer。后三项使用官方 MIT 许可 ONNX 权重和原生 ONNX Runtime CPU 库；PCM 处理和流式接收使用 Rust。参见[模型版本、许可、采样率、衰减和延迟](../../backend/models/nr/README.txt)。源码构建可先运行 `bash tools/fetch-nr-runtime.sh` 恢复固定版本库，再执行 `make package-runtime`。
- `htra_api.py`：HAROGIC 官方 SDK Python 包装，版权归 HAROGIC（海得逻捷）；排除在换证及新生成的源码归档外，厂商再分发／修改条款待核实。
- `libhtraapi.so`：专有二进制，**不在本仓库**，需从 HAROGIC 官方获取
- 官方 [SoapyHarogic](https://github.com/HAROGIC-Technologies/soapy-htra) 修订版 `762dbfa1512c705e1d8b86e820b06e8a10e08a92` 为 **GNU LGPL v2.1 或更高版本**，在构建时下载，不纳入仓库。`tools/soapy-htra-stream-format.patch` 是本项目的兼容补丁，只应用于私有构建副本。
- 可选 [multimon-ng](https://github.com/EliasOenal/multimon-ng) 与 [rtl_433](https://github.com/merbanan/rtl_433) 使用 GPL v2；[OnAir](https://github.com/adswill/OnAir) 使用 GPL v3，并有组件各自条款。它们保持独立进程安装和现有管道／网络接口。安装脚本保留源码构建工具的固定源码／许可证，以及 OnAir 二进制说明和对应源码信息；再分发须遵守上游各项条款。
- `tools/companions/rtty/main.cpp` 保持 **GPL-3.0-or-later**，作为独立编译的进程链接 OnAir RTTY 解码器，排除在自有代码换证范围外；`backend/src/external/rtty.rs` 通过管道通信。参见 [RTTY 安装与回放](../../tools/README.md#independent-rtty-companion)。
- `tools/vendor/ggmorse/`：CW（莫尔斯）解码器来自 Georgi Gerganov 的
  [ggmorse](https://github.com/ggerganov/ggmorse)，**MIT** 协议，保留上游提交
  `7b4822a8`（见 `tools/vendor/ggmorse/LICENSE`）。`backend/cw_build.rs` 将其与本项目的
  `backend/native/ggmorse_bridge.cpp` 桥接代码一起编译进原生服务。

## 维护优先级

| 优先级 | 工作 | 验收条件 |
|---|---|---|
| P2 | 扩展十六信道真实 RF 的频点、模式及主机负载覆盖 | 保留二进制／主机／采集／源／时长／连续性和失败证据；用有界 USB 诊断调查资源压力期间的偶发传输超时，不由单次通过推断其原因已修复 |
| P2 | 补充 blanker 真实音频与并发模型内存证据 | 保留冷启动／跨块脉冲／阶跃解析参考，记录分块影响；区分进程 RSS 与模型独占成本 |
| P2 | 可行时用行为测试替代剩余源码字符串断言 | 等价故障覆盖，而非仅减少文件数 |
| P2 | 验证更多真实 SDK／设备的 Soapy 几何 | 保留分配／坏包注入回归；增加实际位宽／采样率证据，不放宽资源边界 |
| P2 | 验证真实 SDK 永不返回时的进程退出与恢复 | 保留所有者退役／排他回归；单独验证启动器对厂商调用和静态析构的最终停止期限 |
| P2 | 提议退役旧归档兼容时专项复审 | 真实保留文件的兼容证据和明确迁移路线 |
