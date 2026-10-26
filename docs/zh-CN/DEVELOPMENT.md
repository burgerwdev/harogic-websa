# 开发指南

本指南覆盖原生 Rust 服务、共享 DSP、浏览器控制界面和开发工具。
组件边界见[架构](ARCHITECTURE.md)，通信协议见 [API](API.md)。

## 1. 环境、构建与运行

1.9.4 使用原生 Rust PFB/DDC；Python 与保留的浏览器 WASM 产物仅用于离线工具和可选诊断，
不承载生产后端。运行仓库随附产物使用 `./run.sh` 或 `./run.sh --fake`，依赖见
[发布运行要求](../../README.zh-CN.md#运行随附发布产物)。随附内容为
`runtime/linux-x86_64/websad`、本项目 SDK 适配库和 `runtime/frontend/dist/`。
运行要求为 Linux x86_64、glibc 2.44、GLIBCXX 3.4.30、CXXABI 1.3.15 和动态 FDK AAC
（`libfdk-aac.so.2`）。硬件还需要用户自行提供 HAROGIC SDK；SDK 及其依赖均不随附。
较旧发行版或其他架构须重新构建，不承诺跨平台二进制兼容性。

使用 `make build sdk-shim && make package-runtime` 重建并打包发布产物。
单独运行 `make package-runtime` 只复制并 strip 现有产物，不执行编译。服务、适配库和前端产物必须
来自同一发布版本。启动脚本优先使用本地重建结果，其次使用仓库随附产物；
`WEBSA_BIN` 和 `WEBSA_STATIC` 可覆盖相应选择。
数字模式的合成回放还需要仓库中已跟踪的 `tests/fixtures/`；仅复制 `runtime/` 不包含这些
fixture。根目录启动脚本设置工作目录；fixture 默认从该目录下的 `tests/fixtures` 读取，
可通过 `WEBSA_FIXTURES` 覆盖目录。

源码重建使用 Linux、Rust stable、C/C++ 工具链、CMake、pkg-config
和 Node.js 22.22+、24.15+ 或 ≥26（前端测试栈的最低要求；单独 Vite 构建接受 22.12+）。
Python 3.12 或 3.13 用于测试、fixture 和台架工具，不承载生产后端。原生音频解码在构建时
需要 FDK AAC ≥2.0.3，运行时需要其动态库；[工作流](../../.github/workflows/ci.yml)记录了
固定源码版本和安装方法。硬件运行需要用户自行提供厂商 SDK。

在仓库根目录依次执行：

```sh
python3 -m pip install -r requirements-dev.txt -c requirements-lock.txt
make frontend
make native
./run.sh --fake --port 8099 --instance websad-dev
./status.sh --instance websad-dev
./stop.sh --instance websad-dev
```

`make frontend` 调用 `build.sh`，生成 `frontend/dist/`。前端修改后须重新构建，
服务提供的是构建后的资源。`make build` 构建前端和原生服务；`make wasm` 用于显式构建
离线 DSP 产物，不是产品前端构建的前置步骤。

硬件运行先安装厂商 SDK，再执行 `make sdk-shim` 和 `./run.sh`。
`SDK_ROOT`、`SDK_SHIM` 配置 shim 构建，`WEBSA_RUST_IQS_SHIM` 选择运行时 shim。
默认实例为 `websad`，地址为 `127.0.0.1:8080`。用 `WEBSA_INSTANCE` 或 `--instance`
选择实例；status 和 stop 自动读取保存的端点及所有权信息。设备只允许一个进程持有。
服务与硬件配置见[FAQ 说明](FAQ_NOTES.md)和[工具指南](../../tools/README.md)。

Pluto 台架统一入口支持 `radiofax` 和 `wefax`，两者使用同一个流式发射脚本。无需硬件时运行 `python3 tools/pluto_tx.py radiofax --dry-run`；发送图片可加 `--image chart.png --ioc 576 --lpm 120`。图片默认按比例调整高度，`--lines` 可指定行数。AM、反相极性、协议头时长、音频偏移、重复与间隔均见该模式的 `--help`。默认 FM 包含 APT、30 秒定相、1500–2300 Hz 图像音调及 APT 停止；180 LPM 需手动配置接收器。完整页面按设备实际接受的采样率分固定块发送，dry-run 只合成短片头。示例见[工具指南](../../tools/README.md)。

**内存受限主机必须串行、隔离构建。** Rust 构建、Rust 测试和浏览器套件依次运行，
不要使用并行 Make。Cargo `-j 1` 只限制当前调用，不能限制其他构建或浏览器。
Rust 测试保留 `--test-threads=1`。使用主机可用的资源控制和明确的内存上限隔离构建与设备进程。
启动脚本使用脱离终端的后台进程，无需服务管理器。

## 2. 依赖方向与归属

| 区域 | 职责 |
|---|---|
| `backend/src/bin/websad.rs` | 服务生命周期、命令、STATUS 与传输 |
| `backend/sdk/san90_sdk.cpp` | 保留设备 session，串行调用 SDK |
| `backend/src/` | 原生信道化与接收机 |
| `dsp/core/`、`dsp/deepfilter/` | 原生/WASM 共享 DSP crate 与显式浏览器诊断 |
| `tools/reference/` | 离线参考与 fixture 支持 |
| `frontend/src/core/` | 类型、参数槽、结果、运行状态和协议 |
| `frontend/src/dsp/` | 纯前端计算 |
| `frontend/src/render/` | 绘图与重绘接口 |
| `frontend/src/ui/`、`frontend/src/meas/` | 交互、测量生命周期与注册 |
| `frontend/src/audio/` | 音频 worker、worklet 与重采样 |

依赖从编排层指向叶子模块。共享类型、参数槽、纯工具和注册接口不能导入使用它们的上层模块。
前端模块不得存在循环依赖。渲染器只绘图，不写应用状态。采集和计算量大的 DSP 不得阻塞
HTTP/WS 事件循环或浏览器主线程。生产处理使用原生 PFB 路径；离线 Python 参考不能成为运行时回退。

## 3. 参数、结果与显示状态

| 类型 | 归属与规则 |
|---|---|
| 用户参数 | `ui/*State.ts` 中的槽：用户动作调用 `set()`，STATUS 调用 `confirm()`，读取调用 `get()` |
| 仅客户端偏好 | 使用 `authoritative: true` 的槽，不等待后端确认 |
| 迹线和测量结果 | `core/results.ts`，显式 setter，不使用 desired/confirmed 语义 |
| 连接、触发和模式运行状态 | `core/store.ts`，通过显式 setter 修改 |
| 持久化 | 槽的 `persistKey`，不另存 localStorage 副本 |

后端 STATUS 反映设备实际状态。须区分请求值与实际值，不能用待确认的用户值或私有缓存确认参数。
重置须使旧的待处理请求失效。未设置的值须有明确表示，不能用合法频率或电平充当哨兵值。
参数槽应在高频循环外读取一次。模式切换保留偏好，只有明确的交接操作才主动改变偏好。

手动显示 Ref、设备请求与 SDK 实际回读须分别保存。本地窗口为 -200 至 +100 dBm 加 Offset；
设备请求将 Ref 减 Offset 限制在 -160 至 +40 dBm。匹配原始回读不能覆盖所选窗口；同一限幅目标
复用待确认或已接受的硬件请求。待确认编辑不能被过期 STATUS 覆盖，拒绝或超时后恢复已接受的窗口。显式 Auto 是有界操作，手动编辑、
频率或模式变更以及 Preset 须取消旧决策。进入 SDR 时的拟合仅调整显示，不得配置 IQS；
溢出恢复与放置规则见 [FAQ 说明](FAQ_NOTES.md)。幅度偏移只在显示边界应用一次。
实际增益按当前 SDK 模式读取，缺失回读保留为 null；前置放大器 Auto/Off 表示请求策略，
不代表物理开关状态。每个共享显示位置只设一个所有者，独占测量结束后恢复用户原先的选择。
即使采集没有帧，状态变化也必须请求重绘。瀑布映射变化须清空历史，即使已暂停；
电平范围非有限或无效时，须在修改状态前拒绝请求。

## 4. 扩展实现

- 命令与参数：实现原生校验、模式归属和状态转换，同步更新 HTTP/WS 契约及
  `tests/fixtures/contract/schema.json`。前端使用参数槽和现有命令发送器。
  设备限制只通过能力信息发布一份。
- 模式与测量：明确生命周期、采集、恢复和结果归属。通过现有 registry 注册渲染器和
  测量页签，确保入口导入注册模块。断言进入模式后产生可见结果。
- 帧：同步更新原生编码器、保留策略、`core/frames.ts` 解码器和共享黄金 fixture。
  两端都验证布局；`tools/reference/framer.py` 仅提供离线支持。
- 面板：动作放在 `ui/panels/`，使用现有绑定，两种 i18n 语言同时补齐。
  字段与输入框标识通过 `inputForField()`、`fieldForInput()` 解析。
- DSP：使用共享 Rust 内核或纯前端 DSP 模块。保留样本核算、顺序和有界队列。
  SDK 调用留在保留 session 的线程，handle 存储保持稳定；有线程约束的运行库在所属线程初始化。

文档与实现同步更新。注释说明不变量、单位、归属或约束，不记录会话过程、尝试或评审问题编号。

## 5. 测试、fixture 与 CI

断言必须能发现行为错误，覆盖数值关系、边界、状态转换、协议字节和可见结果。
避免照搬实现，或仅通过计数证明渲染器被调用。使用间接指标时，须注明依据及其能证明的范围。
真实采集数据、基准 fixture、模型及厂商来源信息只要仍有使用方就须保留；可重新生成的 fixture
通过现有生成器更新。

| 层次 | 入口与证据 |
|---|---|
| 原生服务与工具 | `./test.sh`：release 模式的后端库与全部二进制、标准 test profile 的集成测试，Python 工具测试、Ruff 和前端 Vitest |
| DSP 内核 | `make wasm-test`：release 模式的原生 Rust 内核测试，包含需优化构建的 FT8 路径 |
| 本地聚合 | `make ci`：内核、服务、工具、前端测试，版本与 fixture 检查，DOM、注册、文档、架构守卫及前端构建 |
| 浏览器 fixture | `make e2e-fake`：原生 fixture 服务、UI smoke、状态回归和视口几何门禁 |
| 硬件 | `make hw-test`：tinySA 驱动的 smoke、命令遍历和 UI 状态回归 |
| 性能 | `make bench`：与 `tools/bench/bench_baseline.json` 比较 |

DSP ABI 文本消息只在当前 push 的结果列表中有效，应在返回消息数后立即读取。后续 push 会替换列表；累计解码计数不会保存消息内容。
后端库、二进制和集成测试分别调用 Cargo，依次运行；合并选择这些目标可能导致共享库产物冲突。集成测试使用标准 test profile，与测试 harness 的 unwind 策略一致；生产 release 产物仍使用 abort。

使用 `python3 -m playwright install --with-deps chromium` 安装浏览器支持，运行浏览器检查前
先构建前端。Fixture 模式证明无 SDK 的应用行为，不证明 RF 正确性、持续采集或硬件恢复。

`make ci` 与 GitHub Actions 是不同入口。工作流分别运行 native、Python tools
（3.12 和 3.13）、frontend、UI smoke 和 architecture job，明确执行
`npx tsc --noEmit`，并在干净环境安装依赖。工作流的浏览器 job 运行 `ui_smoke.py`
和 `state_regression.py`；本地 `make e2e-fake` 还运行 `viewport_baseline.py`。
实际门禁以 [Makefile](../../Makefile)、[test.sh](../../test.sh) 和
[ci.yml](../../.github/workflows/ci.yml) 为准。本地聚合或单个 CI job 都不能代替硬件验证。

## 6. 硬件验证与性能隔离

使用专属设备 session、固定 RF 源及配置、单个预期客户端，停止竞争的构建、基准和
浏览器套件。记录配置、时长、告警、包与样本连续性，以及完整命令。验证原生服务和
实际浏览器界面；帧计数不能单独证明迹线、音频或解码结果正确。
覆盖相关模式切换、重配置和恢复路径。

明确硬件结果的适用范围。合成吞吐、短时捕获和无界面的传输检查不能证明持续 RF 或
全图层浏览器性能。专项采集和接收机检查见[工具指南](../../tools/README.md)与
[架构](ARCHITECTURE.md)。不得将旧结果表述为本次重新验证。

比较性能前恢复干净设备状态，移除无关主机负载。避免逐帧分配和重复计算，保留传输
背压与各流的保留策略。`make bench` 使用保留的基线；解释原生服务结果前须确认其
工作负载和协议仍然适用。`make bench-record` 会重写基线，只能基于有记录、可比较的
测量使用，不能掩盖回归。放宽架构基线也必须说明原因并经评审。

## 7. 故障与发布

原生恢复逻辑负责设备链路故障，进程退出后须用 `make restart` 恢复。实例日志保留错误上下文，UI 和脚本
所需诊断通过 STATUS 暴露。区分告警与采集失败，包括 SDK 告警伴随有效 IQ 的情况。
不得因可恢复告警静默丢数据，也不得将断开的设备报告为健康。

排障时先查看 `./status.sh`（用 `--instance` 选择命名实例）及其输出的日志文件。
画布为空时，先检查前端资源是否重新构建、注册模块是否可达及实际绘制的像素。控件未更新时，先检查 STATUS、
待处理请求和主线程负载。设备忙时，先检查是否被其他程序独占，再考虑重试。

提交信息使用英文 `type(scope): summary`，说明变化及目的。每个提交应独立有效。
发布前运行适用门禁；设备行为需要硬件验证，未验证范围须明确说明。
在 `pyproject.toml` 修改版本后执行 `python3 tools/checks/sync_version.py`；
`--check` 验证前端 package、lockfile 和 HTML 副本。发布验证前重新构建前端。
合并评审后的修改，为已验证版本打 tag，保持源码、文档和发布产物一致。
