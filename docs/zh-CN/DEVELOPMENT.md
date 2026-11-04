# 开发指南

实收准备可运行 `python tools/bench/sdr_capture_observe.py --origin http://127.0.0.1:8080 --expect-service-id <当前服务ID> --seconds 300 --artifacts <新目录>`，先 GET `/api/sdr/receivers` 核对身份。工具只发 GET，拒绝服务身份变化与已有证据目录，保存源时钟／样本推进／告警／录制／负载，不打开 WebSocket 或启动采集；须另有已授权的浏览器／录制消费者。报告仅为观察证据，连接正常或计数前进均不单独证明 RF 接收或物理听感。

设备窗口前完成新版 release 构建；既有进程在明确重启前仍运行旧二进制。替换硬件进程前确认目标实例、N90 占用释放与 RF 来源，另一端口不能隔离同一设备。先恢复一个真实信号，连续观察至少五分钟，检查频谱／调谐、实际输入率、源 epoch／位置及实际听感；逐路增加至三路，完整通带和过渡带须位于同一真实采集范围，再组合 IQ／发布音频／结果录制观察五分钟。停止后独立读取文件，核对 UID／实际率／源边界和接纳／落盘字节。无真实协议源或未试听的项目准确标记未验。合成 `native_multichannel*.py` 会创建／重配自己的服务，不得指向既有硬件实例。


结束的音频任务在 `audio_archive_read` 可用时提供**音频试听**。选择源片段和样本帧起点／长度，载入后使用原生播放／定位／音量控件，也可下载同份Float32 WAV与来源快照。立体声一帧包括两个声道；最多1,048,576帧／60秒。原共享PCM处理保持，实时监听控件独立；隐藏或编辑会释放文件播放。当前校验器先扫描完整任务，长任务载入较慢，但不会累积全部样本。门禁：`python tools/e2e/native_multichannel_audio_playback.py --artifacts /tmp/websa-audio-playback`，使用独立CRC／Zstd及SciPy WAV读取器、双卷单／双声道精确时钟夹具、实际AM／FT8网络录音、真实媒体播放／定位、420px中文／浅色Pop out／Dock、长按鼠标／键盘下载、共用名额取消／TCP停读／损坏、健康录制及SIGKILL前缀。先构建前后端。连续长文件及数字文件解码仍单独推进。

文件调谐：展开“文件重新解调”，输入相对原中心的“频偏 / Hz”，选择模式／滤波后生成音频；频率读数为原中心加频偏，整个边带须由原IQ保留。`python tools/e2e/native_multichannel_tuning.py --artifacts /tmp/websa-file-tuning --sigmf-python /path/to/sigmf-venv/bin/python` 验证四种模式正负分数频偏、双卷相位连续、官方SigMF读取器核对原精确时钟、WAV音调、频带边界、取消／背压及真实420px试听／下载。微小非零频偏和分块无关性另有原生回归。旧服务隐藏频偏控件。

已录 IQ → 波形回放／SigMF → 文件重新解调，可将选区按 AM、USB、LSB 或 NFM 生成音频，用原生音频控件试听，并下载同份 Float32 WAV 与精确 IQ 来源。选择最多60秒及已录通带内的滤波带宽；USB／LSB为单边带宽。48 kHz单声道结果使用从零开始的原生滤波器，关闭AGC／降噪／静噪，原文件与实时接收独立。门禁：`python tools/e2e/native_multichannel_demod.py --artifacts /tmp/websa-file-demod`；使用独立CRC／WBIQ及SciPy WAV读取器、非整数IQ率的合成已知音调、原生实际AM记录、真实鼠标／键盘媒体操作、420px Pop out／Dock及实时录制隔离。执行前构建前后端。本模拟模式切片不包含数字文件解码或连续长文件播放。

已结束音频任务在 `audio_flac_export` 可用时，于“下载音频”旁提供 **FLAC · 24 位**。它将原已发布音频按真实率量化为 signed-24 PCM，Float32 WAV 继续提供原样本位模式。固定比例、饱和统计及未改动的逐帧来源保存于 tar 清单／时间线。纯 Rust `flacenc` 不新增运行时程序或动态音频库。门禁：`python tools/e2e/native_multichannel_flac.py --artifacts /tmp/websa-flac`，开发验证需安装参考 `flac` CLI（实测1.5.0）。独立解码实际AM／FT8发布音频及合成单／双声道量化边界，核对MD5／样本时钟、窄窗原生下载、取消、损坏、TCP停读与恢复前缀。来源及精确转换见 [API](API.md)。

已发布音频持续录制：工作台 → 录制 →“录制音频”，停止后“下载音频”。真实 AUD2 Float32 PCM 在后端保存，浏览器退出后继续；按实际音频输出能力开放，包含数字模式伴随音频。下载包保留各配置／格式片段的 Float32 WAV 与逐帧时钟／原头／配置，浏览器音量、声像和总静音不改变文件。它是实际共享 PCM，不是未增强的协议输入。Opus 和数字文件解码仍待后续。`recording_audio.rs` 复用 `recording_journal.rs` 的有界 CRC／Zstd codec；`receiver_workers.rs` 将实际包配置随 PCM 传到发布点，避免迟到旧帧串用新设置。门禁：`python tools/e2e/native_multichannel_audio.py --artifacts /tmp/websa-audio`，独立 SciPy WAV／系统 libzstd／CRC、实际网络字节、真实浏览器与恢复／故障验证。完整限额与语义见 [API](API.md)。

持续结果录制：信道工作台 → 录制，对实际具有文字／表格／图像／状态输出的处理器使用“录制结果”。同一 UID 的 IQ 与结果可各自独立录制。停止后“下载结果”流式下载包含任务信息和原发布事件 JSONL 的 tar，保存增量图像像素；从录制启动开始保存，已在接收中的图像可能只有部分。浏览器退出后仍继续。门禁：`python tools/e2e/native_multichannel_results.py --artifacts /tmp/websa-results`，验证实际 FT8／SSTV 回放、独立 CRC／Zstd 与 JSONL／tar 读取、420px 浏览器真实操作／下载及 SIGKILL 恢复。不是硬件 RF 或后端音频录制验收。

本指南覆盖原生 Rust 服务、共享 DSP、浏览器控制界面和开发工具。
组件边界见[架构](ARCHITECTURE.md)，通信协议见 [API](API.md)。

单信道预设：信道工作台 → 信号设置 → **频点预设**。保存服务器已确认的频点、IQ与处理设置，选择已保存项后预览并确认载入；同名替换和删除需确认。草稿、启停、监听与录制任务不写入预设。预设跨浏览器及服务重启保留，但重启不会自动应用。门禁：`python tools/e2e/native_multichannel_presets.py --artifacts /tmp/websa-presets`，验证持久保存、冲突／过期预览拒绝、原生录制隔离及420px Pop out／Dock。存储协议见 [API](API.md)。多路原子场景仍未交付。

模式记忆：各信道按模式（含仅 IQ）恢复最近确认的 IQ 通带／过渡带／最低率、解调带宽、共享 PCM 增益、AGC 和静噪。首次使用某模式采用其音频默认值。恢复只更新表单，保留频率／名称／接收启停，仍需检查并显式应用；不记忆草稿或被拒绝的变更。同一标签页刷新后仍保留，其工作台窗口与 Pop out／Dock 共用记忆；删除 UID 或更换服务代次时清除。这不是服务器持久化或跨标签页同步。预览后编辑表单会移除旧应用按钮，需重新检查。

多路频点须同时落入共享采集，完整通带和过渡带也须留在范围内。工作台的越界拒绝／预览会显示当前采集范围和该信道所需范围，指引先在主 SDR 面板调整共享中心或采集带宽，再重试；重试本身不会扩大采集。例如101.7MHz中心、3.125MHz采集仅覆盖100.1375–103.2625MHz，无法接收105MHz FM；显式将共享中心设为103.35MHz、可用带宽设为6.25MHz后可覆盖这两个频点。共享采集变更影响全部信道，主面板的中心输入还会调谐C1；只调整共享窗口时使用省略`listen`的`SET_SDR`，并核对各UID频率。

三路日常工作流门禁：`python tools/e2e/native_multichannel_workflow.py --artifacts /tmp/websa-daily-workflow`。组合 AM 音频录制、FT8 结果录制、健康 IQ 录制、本地监听、确认模式记忆／刷新、预设复用、启停、复制与窗口／页面关闭，再用独立 CRC／Zstd 与 WAV 读取器验证实际文件。来源为合成／回放；RF、物理音频和长期运行仍须另验。

## 1. 环境、构建与运行

服务使用原生 Rust PFB/DDC；Python 与保留的浏览器 WASM 产物仅用于离线工具和可选诊断，
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

可选外部接收器通过
`tools/setup-sdr-decoders.sh --prefix "$HOME/.local/share/websa/decoders"` 独立安装。
脚本固定并校验 multimon-ng、rtl_433 和 OnAir，串行构建前两者，保留源码与许可证，
不更改系统软件包或运行中的服务。启动自己的 WebSA 实例前加载生成的 `env.sh`，其中导出
`WEBSA_MULTIMON_PATH`、`WEBSA_RTL433_PATH` 和 `WEBSA_ONAIR_SERVER`。
提供者探测结果在服务生命周期内缓存；更改依赖后须重启该实例。
主 runtime 包不包含这些 GPL 工具。OnAir portable 仅限 Linux x86_64；脚本在二进制
许可证说明旁记录对应源码 URL 与校验和。
验证 fixture 时，将规范化的交错 f32le IQ 放在
`$WEBSA_FIXTURES/external/<mode>.f32`，采样率须等于模式目录声明的精确输入速率。
缺少文件时拒绝配置。FT4/WSPR 需要与 UTC 对齐的实时输入，拒绝无时间对齐信息的文件及循环
fixture 回放；仅按实时速率播放录音不能证明时隙对齐。

GPS L1 C/A 的独立 GNSS-SDR 进程通过 `tools/setup-gnss-decoder.sh` 安装；原生 Rust 桥接器
通过 `cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin websa-gnss` 构建，
将其安装到解码器前缀的 `bin` 目录后加载 `gnss-env.sh`。安装脚本支持 `--dependency-prefix`
指定本地依赖开发文件，支持 `--python` 指定仅用于构建的 Python/Mako 虚拟环境；生产桥接器
不依赖 Python。输入是 SDR IQ，不是设备 SDK 内置 GNSS 状态。参见
[GPS 安装和真实录音来源](../../tools/README.md#gps-l1-ca-from-sdr-iq)。

GPS 在适配前要求至少 2.6 MSps，提供者 FIFO 输入则为精确 4 MSps；居中的
3.90625 MSps 采集可在保留配置带宽和滤波过渡带的前提下适配。CTTC 真实录音回放
已经验证。435 MHz Pluto/SAN-90 冷启动台架以 gain −30 dB、3.90625 MSps 采集，
在 63.41 秒后得到四颗卫星支持的有效 PVT，与合成目标的水平误差为 3.653 m，
未报告接收器错误。该结果验证了所测试的 GPS 频移台架路径；被动天空接收仍未验证。
正常与反向 RTTY
均已由独立 OnAir 伴随程序实现：运行 `tools/setup-rtty-decoder.sh` 后重启服务，
默认解码器目录会被自动发现；自定义 `--prefix` 需加载其 `rtty-env.sh` 或设置
`WEBSA_DECODERS_PREFIX`。两种 45.45 baud/170 Hz 模式均已通过离线 fixture
和 435 MHz Pluto/SAN-90 台架解出已知文本。参见
[RTTY 安装与回放](../../tools/README.md#independent-rtty-companion)。

源码重建使用 Linux、Rust stable ≥1.95、C/C++ 工具链、CMake、pkg-config
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

原生 release profile 使用优化级别 3、16 个 codegen unit 和增量编译，关闭 LTO。
保留 `backend/target/` 可复用编译缓存。profile 或工具链变更会重建依赖；
release 二进制和测试 harness 使用不同的 panic 策略。

保持 `make build` 无 warning，并保留编译器和 Vite 的默认诊断。
CSS 字体路径使用 `frontend/public/` 对应的 `/fonts/`，由 Vite 加上 `/static/dist/`
基础路径。翻译资源生成独立且立即加载的 `i18n` 分块，使主包低于 Vite 默认大小阈值，
同时保持语言初始化行为不变。

硬件运行先安装厂商 SDK，再执行 `make sdk-shim` 和 `./run.sh`。
`SDK_ROOT`、`SDK_SHIM` 配置 shim 构建，`WEBSA_RUST_IQS_SHIM` 选择运行时 shim。
默认实例为 `websad`，地址为 `127.0.0.1:8080`。用 `WEBSA_INSTANCE` 或 `--instance`
选择实例；status 和 stop 自动读取保存的端点及所有权信息。设备只允许一个进程持有。
服务与硬件配置见[FAQ 说明](FAQ_NOTES.md)和[工具指南](../../tools/README.md)。

Pluto 台架统一入口支持 `radiofax` 和 `wefax`，两者使用同一个流式发射脚本。无需硬件时运行 `python3 tools/pluto_tx.py radiofax --dry-run`；发送图片可加 `--image chart.png --ioc 576 --lpm 120`。图片默认按比例调整高度，`--lines` 可指定行数。AM、反相极性、协议头时长、音频偏移、重复与间隔均见该模式的 `--help`。默认 FM 包含 APT、30 秒定相、1500–2300 Hz 图像音调及 APT 停止；180 LPM 需手动配置接收器。完整页面按设备实际接受的采样率分固定块发送，dry-run 只合成短片头。示例见[工具指南](../../tools/README.md)。

`tools/pluto_tx.py` 还分派 DTMF/APRS/AIS/FT4/WSPR 和合成 GPS。数字模式与 GPS
使用共享 `pluto_radio.Radio` 的 `open_stream`、`prime_stream`、`push_chunk`
和 `drain_stream` 流式 API，队列包含四个缓冲区。GPS 源通过
`tools/setup-gps-simulator.sh` 安装。共享发射层会在发射前开启被关闭的 TX 本振，
退出时先销毁发射缓冲，再恢复原本振电源状态。GPS CLI 只允许请求 435 MHz 载波，LO 回读
与其偏差不得超过 100 Hz。波形生成前先协商实际采样率，要求回读值与请求值相差
不超过 20 Hz 且能被十整除，随后严格按该实际速率生成。默认 TX 采样率为
2.6 MSps，与接收提供者的精确 4 MSps 输入分开。GPS gain 默认仍为 −70 dB；
所选台架值须显式传入 `--gain -30`。发射完成本身不能证明 GPS 射频解码通过。
合成场景、接收端显式启用方式和连续性限制见
[发射指南](../../tools/README.md#rf-transmit-and-capture)。

**内存受限主机必须串行、隔离构建。** Rust 构建、Rust 测试和浏览器套件依次运行，
不要使用并行 Make。Cargo `-j 1` 只限制当前调用，不能限制其他构建或浏览器。
Rust 测试保留 `--test-threads=1`。使用主机可用的资源控制和明确的内存上限隔离构建与设备进程。
启动脚本使用脱离终端的后台进程，无需服务管理器。

多信道开发原型通过 `npm --prefix frontend run dev:workbench` 启动，打开
`http://127.0.0.1:5186/static/dist/workbench.html`。此独立 Vite 页面使用 fixture adapter，
不连接原生服务、不启动音频或记录文件，生产构建不包含该入口。状态契约与后端离线实验见
[MULTICHANNEL_DRAFT](MULTICHANNEL_DRAFT.md)。浏览器回归：
`python tools/e2e/multichannel_workbench.py`（需可用的 Playwright Python 环境）。

原型支持在频谱/瀑布右键选点加入监听，也可点右侧“＋ 监听频点”输入 MHz/kHz/Hz。
有空位可新建听音信道，满路数时显式选择已有听音信道；预览记录分段影响，服务确认后才开启本地路由。
同频复用不重新配置接收，当前编辑目标保持不变。该入口仍为 fixture，不产生真实声音。

无硬件原生实验（默认 0.05 秒合成输入；不是容量验收）：

```sh
cargo run --offline --release -j 1 --manifest-path backend/Cargo.toml --features iq-experiments --bin multichannel_experiment -- /tmp/websa-multichannel-experiment 0.05
```

产出 `report.json` 和实验 `.wbiq` 文件，保留 CF32 位模式与实际速率。依赖 Zstd 已在本机缓存时可使用 `--offline`。

生产 SDR → **信道工作台** 打开原生接收工作台。构建前端/原生后，可运行 `./run.sh --fake --port 18139 --instance p3-dev` 启动独立合成服务，以 `./stop.sh --instance p3-dev` 仅清理该实例。`python tools/e2e/native_multichannel.py` 自行启动/清理独立服务，验证实际 IQ 协议和窗口生命周期；需要 Playwright 与 websockets。硬件 RF 实收须单独验证。

P4 监听/解码门禁：`python tools/e2e/native_multichannel_processing.py --artifacts /tmp/websa-p4-processing`。
脚本拥有独立合成/回放服务，检查实际浏览器 PCM 的本地增益/声像、Solo/跟随、FT8 结果、SSTV 像素和窗口生命周期。
生产工作台的“信号设置”页使用实际模式/provider 能力；“本地扬声器”控件不修改共享 PCM。原有后端音量通过“共享音频增益”显式配置。

连接恢复与紧凑布局门禁：`python tools/e2e/native_multichannel_recovery.py --artifacts /tmp/websa-p4-recovery`。
Chromium 真实切断并重连 UID 音频 socket，确认实际输出 PCM 恢复，并操作表单、窄屏弹窗、Dock、信道退役及槽复用。
生产工作台按实际能力开放 IQ 记录，隐藏未开放的预设/场景操作，IQ 参数和共享音频设置可展开；窄窗仍可编辑。跟随模式下直接点“监听”切换为手动混听，保留编辑目标。
主面板“信道工作台”入口只聚焦工作台，保留其他独立弹窗；召回全部窗口和 Dock 仍是明确的布局操作。

主面板在监听标签旁显示当前信道；工作台旁的左右按钮切换接收信道，调谐与解调控件跟随该信道，切换本身不改频率。`＋` 将当前频点与解调设置复制到独立信道；路数已满时先选择要覆盖的信道。主频谱和瀑布以不同色通带标出其他信道，以浅色虚线显示同步鼠标预览，离开图面即清除。验证：`python tools/e2e/sdr_channel_shortcuts.py --artifacts /tmp/websa-channel-shortcuts`。
完整语义及尚未完成的共享样本块迁移见 [MULTICHANNEL_DRAFT](MULTICHANNEL_DRAFT.md)。

进入 SDR 自动开启瀑布图，频谱与瀑布各占一半；Preset 恢复此布局并选中 C1。瀑布点选调谐当前信道，开启放大后可拖框选择两张图共享的显示范围。工作台将接收开关、复制、删除整齐排在一行，各信道“更多操作”也提供复制、删除。频率、接收音频参数、本地音量和声像相同的信道只播放一路；主面板调到其他频点后，自动恢复两路独立混听。此本地规则不合并接收或录制任务。

工作台提供“信号设置、录制、实时输出”三个页签。唯一模式选择包括“不解调 · 仅接收 IQ”；监听与 IQ 录制是独立操作。点击信道名称在原处编辑，“独立窗口”打开固定设置；“信号信息／查看结果”查看已运行处理器的读数。按钮提供用途提示。“实时输出”列出实际 WebSocket 连接，折叠的本页 IQ 检查只统计传输，不保存文件。

工作台/Decode 实时交互门禁：`python tools/e2e/native_multichannel_ui.py --artifacts /tmp/websa-multichannel-ui`。
脚本拥有独立合成/回放服务与记录目录。Chromium 跨状态更新实际按住鼠标和 Space，检查控件未脱离 DOM、草稿/搜索选区/滚动保留、实际 FT8 消息与 SSTV 像素，并在后端 IQ 记录继续时操作 420px Pop out/Dock。还验证真实 AM 波形、同 UID 同时监听／IQ 录制／筛选 CSV 与 JSONL 下载，以及弹窗内真实解码 PNG 下载；不构成 RF 硬件验收。

P5 IQ 记录门禁：`python tools/e2e/native_multichannel_recording.py --artifacts /tmp/websa-p5-recording`。脚本隔离存储和服务，独立 CRC32/libzstd 读取器逐块对照实际 IQ 网络字节，验证变率/换频、停止排空、真实鼠标跨轮询点击、420px 弹窗、Dock、关闭浏览器持续记录和 SIGKILL 后恢复。仅为合成源验证，RF 和长期磁盘吞吐须另验。

`WEBSA_RECORDING_DIR` 指定本实例后端存储根；默认 `$XDG_DATA_HOME/harogic-websa/recordings/<port>`（未设置时使用 `~/.local/share`）。目录有独占文件锁，多实例应使用不同目录。启动时流式恢复中断/失败任务的校验前缀与 JSONL 索引；不自动续录。普通工作台显示 IQ 开始/停止、状态和大小，压缩与时钟详情折叠。服务发布 `iq_archive_read` 后，已完成／失败的 IQ 任务提供“波形回放 / SigMF”，旧服务／退役 UID 的任务在“历史记录任务”中可访问。选择片段及样本范围，可查看 I/Q、定位、按实际率播放波形或下载标准 SigMF。每范围限 1,048,576 个样本、4096 块和 4 MiB 来源元数据。协议输入音频、Opus 及文件重解调回放尚未开放。结果窗口可以将已保留消息按筛选条件导出 CSV 或 JSONL（含全部分页），并将选中图像保存为 PNG；它们是有界浏览器会话快照，持续录制仍由后端任务承担。JSONL 区分接收时间与协议 UTC，并保留源位置字符串。未完成图像的文件名带 partial，PNG 导出 RGBA 内存限制为 20 MiB。仅文本／表格模式或已有文本结果显示筛选工具，音频窗口不显示消息筛选或清空操作。AM/WFM 复用旧音频波形绘制器；CW 显示原生速度、音调和调谐偏移；DRM 显示电台／节目及锁定信息。

每路滑块控制当前编辑信道在本浏览器中的监听。先开启该路“监听”，音量显示百分比，声像显示左／中／右位置；声像需要立体声输出。编辑另一信道不会切换正在听的组合。未监听、故障或停用时禁用滑块，总静音／独听屏蔽保留明确状态。通用操作说明已移出主要界面，帮助使用悬停提示，来源细节折叠。

已录 IQ 可选“整段下载”，或填写范围后“下载 SigMF”。发布 `iq_archive_export` 的服务采用浏览器原生下载，CF32 数据文件小于 POSIX tar 的 8 GiB 上限，波形预览仍限 1,048,576 个样本；旧服务保留原小范围下载能力。回归入口：`python tools/e2e/native_multichannel_stream_export.py --artifacts /tmp/websa-stream-export --sigmf-python /path/to/sigmf-venv/bin/python`（`sigmf==1.6.1`）。它验证明确重复的离线 32 MiB 素材、真实扬声器控件／PCM、取消、损坏、慢 TCP 消费者、记录隔离和独立 SigMF 解析；不代表 RF 采集或实际声卡验收。

已结束的多片段 IQ 任务在 `iq_archive_collection` 可用时提供“全部片段”，一次下载标准 SigMF 集合，各片段保持独立率、调谐和源时钟。门禁：`python tools/e2e/native_multichannel_collection.py --artifacts /tmp/websa-collection --sigmf-python /path/to/sigmf-venv/bin/python`。覆盖实际原生记录与网络 IQ 比对、两种率的 32 MiB raw／Zstd fixture、解压后官方 `SigMFCollection` 直接载入、SHA-512 元数据引用、逐记录 Schema／样本校验、取消、后段损坏、健康记录、420px 下载和两段 SIGKILL 恢复。官方 1.2.6 集合 JSON Schema 仍只接受旧 tuple，而规范推荐 name／hash 对象，sigmf-python 1.6.1 也直接读取对象。门禁保留此上游 Schema 差异，不修改读取器／Schema；各 Recording 使用官方 Schema 验证，Collection 通过官方读取器直接校验。

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

信道工作台以服务快照为已确认状态，由会话串行提交信道变更、合并重复删除，并将排队编辑及迟到回复绑定到服务代次。
删除期间阻止该信道后续启动；服务重启或会话释放使旧任务失效。草稿只属于对应视图与信道。
主界面恢复监听须同时满足匹配的命令确认与健康快照，后续用户明确选择的监听状态优先。
`tools/e2e/multichannel_audio_settings.py` 在独立模拟服务中验证这些控件、连续删除、紧凑布局及播放恢复。

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

生命周期测试需要本机构建的 `backend/target/release/websad`；Python CI 作业与 `./test.sh` 均在 pytest 前构建它。随附 runtime 有独立的宿主 ABI 要求，不作为 CI 的测试可执行文件。

| 层次 | 入口与证据 |
|---|---|
| 原生服务与工具 | `./test.sh`：release 模式的后端库与全部二进制、标准 test profile 的集成测试，Python 工具测试、Ruff 和前端 Vitest |
| DSP 内核 | `make wasm-test`：release 模式的原生 Rust 内核测试，包含需优化构建的 FT8 路径 |
| Soapy 补丁 | `make soapy-check-patch`：helper shell 语法及固定版本的补丁适用性；需要 curl + patch，无需 SDK |
| Soapy 流 | `make soapy-check-stream SDK_ROOT=/opt/htraapi`：使用真实厂商头文件、SoapySDR 开发文件及 HTRA 桩检查实际补丁驱动；无需厂商库或设备 |
| 本地聚合 | `make ci`：内核、服务、工具、前端测试，无 SDK 的 Soapy 补丁检查，版本与 fixture 检查，DOM、注册、文档、架构守卫及前端构建 |
| 浏览器 fixture | `make e2e-fake`：原生 fixture 服务、UI smoke、状态回归和视口几何门禁 |
| 硬件 | `make hw-test`：tinySA 驱动的 smoke、命令遍历和 UI 状态回归 |
| 性能 | `make bench`：与 `tools/bench/bench_baseline.json` 比较 |

DSP ABI 文本消息只在当前 push 的结果列表中有效，应在返回消息数后立即读取。后续 push 会替换列表；累计解码计数不会保存消息内容。
后端库、二进制和集成测试分别调用 Cargo，依次运行；合并选择这些目标可能导致共享库产物冲突。集成测试使用标准 test profile，与测试 harness 的 unwind 策略一致；生产 release 产物仍使用 abort。

使用 `python3 -m playwright install --with-deps chromium` 安装浏览器支持，运行浏览器检查前
先构建前端。Fixture 模式证明无 SDK 的应用行为，不证明 RF 正确性、持续采集或硬件恢复。

密度 smoke 场景先打开“迹线 → 更多”，再操作 Persist。检查 Off 关闭密度层、
保存 `rta-halflife`，并在切换模式后保留；余辉设置值以秒为单位表示半衰期。

`make ci` 与 GitHub Actions 是不同入口。工作流分别运行 native、Python tools
（3.12 和 3.13）、frontend、UI smoke 和 architecture job，明确执行
`npx tsc --noEmit`，并在干净环境安装依赖。工作流的浏览器 job 运行 `ui_smoke.py`
和 `state_regression.py`；本地 `make e2e-fake` 还运行 `viewport_baseline.py`。
实际门禁以 [Makefile](../../Makefile)、[test.sh](../../test.sh) 和
[ci.yml](../../.github/workflows/ci.yml) 为准。本地聚合或单个 CI job 都不能代替硬件验证。

`make ci` 和托管 CI 均运行 `soapy-check-patch`，对 helper 执行 `bash -n`，并检查补丁
能否应用于固定的上游源码版本。该门禁需要 `curl`、`patch` 和标准 shell 工具。独立的
`soapy-check-stream` 门禁还需要 `pkg-config`、`realpath`、C++ 编译器、SoapySDR 开发文件
以及 `SDK_ROOT` 下的真实厂商 SDK 头文件。它只链接 SoapySDR，用桩函数提供 HTRA 符号，
检查流契约，不能据此认定 RF 采集或客户端应用已通过验证。

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
