# VSA 路线图 — 剩余工作

把 [VSA_FEASIBILITY.md](VSA_FEASIBILITY.md)(已冻结的分析)推进成可用模式的活清单。每条都带上
驱动它的实测数字,这样"为什么要做"永远不用猜。状态标记:**已做**、**待做**、**受阻**(需要本分支
之外的东西)、**未验证**(目前哪儿都没测过)。

## 1. 当前状态

* **[已做]** 可行性分析、探针套件与实测数据:[VSA_FEASIBILITY.md](VSA_FEASIBILITY.md)、
  `tools/vsa_probe/`(见其 `FINDINGS.md`)。
* **[已做]** 分析中提出要修的那条设备链路崩溃(把失效句柄交回厂商库)以及 supervisor 的
  启动崩溃护栏 —— 已随 **v1.7.4** 发布,master 也已合入本分支。
* **[已做]** 日志滚动策略真正生效(v1.7.4)。
* **[已做]** Phase 1 会话层:`SET_MODE 'vsa'` 已端到端可用。`VsaSession` + `VsaParams` 遵循
  `MeasurementSession` 契约,`SET_VSA` 已进命令表,`FakeVsaSession` 支撑假后端,真机 SAN-90 一次
  抓满 2^24 样本帧、0 包错误(`tools/vsa_probe/vsa_service_check.py`)。
* **[已做]** 本轮范围内的 Phase 1 与 Phase 2:Tier1 测量模块、带按帧类型 latest-wins 的 `VSAD`
  帧、Tier2 解调链路、VSA 前端(模式、设置组、测量面板、带旋转控件的星座)、测试与假后端 UI 冒烟,
  以及一次真机对账(`tools/vsa_probe/FINDINGS.md` 第 15 节)。Phase 3(SDK 子进程)与本分支合并仍
  不在范围内;仍未验证的项见第 6 节。

## 2. Phase 1 — Tier 1 端到端

### 2.1 IQS 复用层

* **[已做]** `measurements/iqs.py` 拥有 IQS 管道(profile、模式重置、配置后丢弃、单包取数、
  瞬态/致命分类、卡死判定),`SdrSession` 改为调用它;SDR 的恢复*动作*仍留在会话里,因为它要
  重建厂商 FFT/DDC/解调链。验证:`tests/test_iqs.py`(16 例,无需厂商库)+ 一次真机 SDR 流:
  361 包、0 错误、46 帧频谱、2.46 s AM 音频(单音 994.7 Hz)。
* **[已做]** 共享层使用 `web_sa/hardware/sdk_bindings` 的 `IQStream_TypeDef`(728 字节);
  厂商封装里的副本少 8 字节,SDK 会在每个包上越界写入。
* **[已做]** 约 0.25 s 的配置后丢弃保留在流式通路上:实测不丢弃时 10824/10824 次取数失败,丢弃后
  为 0。`FixedPoints` 抓帧则**不得**丢弃 —— 见第 7 节的配方。
* **[已做]** `-9` 恢复复用而非另写:多速率 soak 中 6 次解调因子切换有 2 次会把流永久卡死;VSA 两条
  通路都按共享判定原地重配,超过 `RECOVERY_LIMIT` 次则升级为 worker 重启。
* **[待做]** 窄带处理一律复用已有实现、不另写 DSP:通道化用 `demod/ddc.py`,流式 FIR/重采样/AGC 用
  `demod/filters.py`。(Tier1 已经复用共享 panadapter `demod/spectrum.py` —— 与 SDR 显示同一套
  加窗 FFT。)
* 约束:SDR 行为不得改变(以其现有测试 + 一次真机 SDR 流为证)。

### 2.2 会话、参数、模式

* **[已做]** `VsaSession`,遵循 `MeasurementSession` 契约(enter/exit 带配置快照、`step`、
  `health`、`request_stop`、`pacing`、`reconfigure`),位于 `web_sa/measurements/vsa.py`。
* **[已做]** `VsaParams` 作为与 `SwpParams`/`RtaParams` 并列的模式私有块,重入时恢复
  (见 [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md))。真机验证:退出 `vsa` 后扫频中心/跨度精确还原。
* **[已做]** 把 `'vsa'` 注册进 `measurements/__init__.py` 与 `web_sa/web/commands.py` 的
  `SET_MODE` 选项表,并新增 `SET_VSA` 与 `NOT_IN_VSA` 护栏集合。
* **[已做]** `FakeVsaSession`,让该模式在无硬件时也能测试与演示(假后端正是 CI 与 UI 冒烟
  使用的路径)。

### 2.3 Tier1 测量

* **[已做]** `web_sa/demod/vector.py`:Welch 频谱、功率-时间、CCDF(附闭式 Rayleigh 参考)、
  频谱图与经载波/定时校正的星座云,并由 `SET_VSA measure` 经单一入口选择要跑哪项。它需要的符号级
  估计器在隔壁 `web_sa/demod/digital.py`(`fft_resample`、Oerder-Meyr 符号率、定时及其半符号
  消歧、M 次方 CFO、RRC 匹配滤波、标称星座);判决级部分在 Phase 2 并入。
* **[已做]** 统一一种绝对 dBm 约定并只写一次:`10*log10(mean|v|^2/50)+30` 配合厂商
  `IQS_ScaleToV` —— 不加额外的 3 dB 带通因子。经服务端对已知源复测(tinySA CW 100.2 MHz、
  −25.0 dBm、`RefLevel 0`、抽取 16、2^18 点):均值 −25.24 dBm、单音电平 −25.2 dBm、峰值 bin
  −28.27 dBm(窗 ENBW 2.00 bin)、噪声密度 −138.9 dBm/Hz。参考跟踪器用的就是这个**单音电平**;
  峰值 bin 单独上报,因为逐 bin 迹线对 CW 会低约 3 dB;而"没有两个电平"的迹线 duty 报 1.0。
* **[已做]** 每项测量的耗时已记录在模块 docstring(默认 2^17 捕获与深 2^20 帧;单核、占实时
  百分比):频谱+电平 21/6、功率-时间 23/7、CCDF 57/21、频谱图 136/44、星座 389/181。探针套件在
  100k 点块上的 22/3/25/120/316 % 量级一致;其 CCDF 用的是块平均包络,与 Rayleigh 不可比。
  仅限抓帧的两项(`spectrogram`、`constellation`)在流式视图下直接拒绝,而不是静默丢弃。
* **[已做]** 当盲符号率意味着每符号不足 4 点时,星座会带 `sps_too_low` 标记(实测 sps 2 时估计
  崩溃);四重相位模糊已在界面上:面板带旋转控件、显示 `resolved_by · rotation`,未确定时给出提示。
  随驱动电平变化的 DC(0.2–1.6 dB)是文档化的输入注意事项而非界面功能 —— 会话保持探针验证过的
  DCC 默认值。

### 2.4 采集通路

* **[已做]** 先捕获再分析:按所需深度取一帧 `FixedPoints`,分析后发布,随后立刻装上下一帧
  (抓帧视图上报 `busy` 与 `progress`,不伪装实时)。深度到 2^24 点零丢包,探针与服务两侧都验证;
  取数约等于实时(逐帧实测 1.00–1.01 倍;经服务端到端 2^24 为 1.22 倍;短到受 USB 带宽限制的帧
  可达约 2.5 倍)。
* **[已做]** 流式 Tier1(频谱/瀑布):`Adaptive` 配同款丢弃,显示刷新 ≤20 Hz
  (`PAN_MIN_INTERVAL`)。Welch(25 %)放得下;频谱图(120 %)放不下,必须归入分析步骤(2.3)。

### 2.5 帧协议

* **[已做]** `measurements/framer.py` 的 `VSAD` 帧:float32 `(rows, cols)` 矩阵(点云是
  交织 I/Q、功率迹线与 CCDF 是 `(x, y)`、频谱图是矩阵)、可选理想点阵、头部 5 个标量(符号率、
  载波偏差、定时、EVM、SNR),以及由 `VSA_MEASURE_KEYS` 命名的**位置式**测量块(EVM/MER/SNR 在
  Phase 2 之前为 NaN)。`decode_vsa` 仅靠 NumPy 可解,声明尺寸与长度不符的帧直接拒绝;
  `vector.frame_payload` 产出适合显示的切片(否则 2^18 点抓帧会有上万个点),抓多深帧都有界。
* **[已做]** 在 `web_sa/web/client_stream.py` 登记:latest-wins 是**按帧类型**的 —— 一次抓帧会
  同时发出频谱(RTAF)与测量(VSAD),共用一个槽位就会互相挤掉。慢客户端只拿到最新点云,单元测试
  与真机 2 s 停顿都验证了这一点。
* **[已做]** 金标 fixture `tests/fixtures/frames/vsa.bin` + manifest 条目,由生产编码器生成,
  两侧都有断言(`tests/test_frame_fixtures.py`、TS `frames.test.ts` → `decodeVsad`)。
* **[已做]** 频谱/瀑布视图原样复用 `RTAF`;一次抓帧同时发两种帧,点云不进 `STATUS.vsa.last`。

### 2.6 前端

* **[已做]** VSA 模式开关(`#btn-mode-vsa`,`graphMode` 接受 `vsa`)、复用现有 RTA 画布的
  Tier1 频谱/瀑布(RTAF 原样复用),以及一个绘制 VSAD 帧所承载测量的面板:星座云对理想点阵、
  功率-时间迹线、CCDF 曲线、频谱图(同一块画布,因为四者都是 float32 矩阵 —— 见
  `core/vsaState.ts`)。
* **[已做]** 星座面板掌管相位旋转控件:它通过 `SET_VSA` 写 `phase_rot`,显示
  `resolved_by · rotation`,并说明是三种答复中的哪一种(前导、用户、或尚未确定)—— 模糊绝不
  静默;未确定时面板给出提示。
* **[已做]** VSA 设置组:中心、抽取、深度、视图(capture/stream)、测量项、调制、滚降、符号率与
  旋转。仅限抓帧的测量在流式视图下会被*禁用*而不是等后端拒绝;进入 VSA 时先调到用户正在看的
  标记/中心,再用一条 `SET_VSA` 下发整组几何(连续两次重配有实测卡死风险)。
* **[已做]** 参考电平继续使用共享的 Ref 组,而且控件现在说明了它是什么:扫频/RTA 是显示刻度顶值,
  SDR/VSA 是 `RefLevel_dBm` *输入增益* —— 实测线性范围写在面板与 tooltip 里(Ref 0 dBm 时线性到约
  −20 dBm,−25 dBm 单音读 −25.2 dBm;到 −15 dBm 会低读 3.9 dB)。深度/抽取列表就是设备接受的那些,
  没有新增客户端自造的限值。
* **[已做]** 双语 i18n(`core/i18n/dict.vsa.ts`)与面板内的提示行。

### 2.7 Phase 1 的测试

* **[已做]** 用合成 IQ 断言已知答案的单测:`tests/test_vector.py` 固定突发占空比(0.500)、
  噪声 CCDF 对闭式 Rayleigh(最大偏差 < 0.01)、符号率误差(< 1 Hz)以及星座云的尺度/CFO/定时,
  `tests/test_digital.py` 固定 EVM 对匹配滤波界、定时、载波、SER 与模糊处理。
* **[已做]** 会话生命周期:模式私有状态快照/恢复、`health`、`SET_MODE` 校验表
  (`tests/test_vsa_session.py`,15 例,不需要厂商库)。
* **[已做]** 假后端端到端:进入/退出 `vsa` 且不干扰其它模式
  (`test_fake_backend_round_trips_through_vsa`)。
* **[已做]** 一个在画布空白或出现 JS 错误时就失败的 UI 冒烟:Playwright 假后端冒烟
  (`tools/e2e/ui_smoke.py`、`make e2e-fake`)切入 VSA,断言面板显示、频谱画布仍在绘制、星座面板
  被绘制(实测 1893 个非背景像素)、读数点名被请求的测量项、指标带数值、模糊被上报、换一种测量后
  曲线上屏、返回扫频正常,且没有页面错误。

## 3. Phase 2 — Tier2 解调

* **[已做]** 链路落在 `web_sa/demod/digital.py`(`demodulate`):符号率、定时、载波恢复、判决、
  EVM/MER/SNR、SER/BER 与格雷码符号表。合成 QPSK/16QAM 在 8–30 dB 实测:从 12 dB 起
  EVM/理论**从不超过 1.05**(本路线图要求的上界),多种子中位数 0.99–1.00 —— 链路就压在该界上。
  字面下界(≥ 1.00)故意不写成断言:理论值是期望值,读到 0.99 只是噪声抽样有利,不是缺陷。8 dB 时
  五个实现里有一个读到 1.15,因为该点 EVM 估计器自身方差约 10%。有前导解旋转时 SER/BER 为 0、
  载波总误差 ≤ 0.03 Hz。
* **[已做]** 逐符号 PLL 已被替换:`track_carrier` 每遍用判决做参考、拟合一条相位/频率直线
  (向量化、三遍、剔除离群)。实测 4000 符号 2.3 ms、65000 符号 50 ms,比探针的循环**快 10 倍**,
  且已不再是耗时主因 —— 主因是 FFT 各阶段与匹配滤波。
* **[已做]** 四重相位模糊被显式处理:`resolve_ambiguity` 要么匹配已知前导(上报四个候选各自的
  SER,只有一个是 0),要么施加用户旋转并上报(`resolved_by` = `reference`/`user`/`none` 外加角度);
  既无前导也无用户旋转时明确报 `none`,绝不静默旋转。
* **[已做]** 不可能的输入以错误码拒绝而不是给错数字:`sps_too_low`(盲符号率在每符号 < 4 点时
  崩溃)、`no_symbol_line`、`silent`、`too_short`;`tier1_cloud` 用 `sps_too_low` 标出同一情况。
* **[已做]** 定时阶段改为判决辅助:纯盲的 |x|² 估计只有 30 dB 时才到 0.05 样本(8 dB 平均 0.31),
  因此 `refine_timing` 直接最小化判决导向 EVM 随采样相位的变化。实测 8–30 dB 最差 0.026 样本,
  30 dB 时 0.001。
* **[已做]** 退化条件记在测试看得见的地方:上面的 8 dB EVM 离散度、sps < 4 崩溃,以及第 7 节的
  时钟场景(CW 载波没有符号谱线,Tier1 绝不把盲符号率当测量值上报)。
* **[待做]** 滚降极端值(0.1–0.9)与超过符号率约 1 % 的载波偏移,探针测过但还没有单测固定。

## 4. Phase 3 — 进程分离(**不在本轮**)

* **[已做]** 第 1 步:已判定失效的句柄不再交回厂商库;supervisor 在连续启动崩溃后停止
  (v1.7.4)。
* **[待做]** 第 2 步:把 SDK 会话(打开/配置/取数)挪进专用子进程,让原生崩溃的代价是一次
  捕获而不是整个 Web worker。
* **[待做]** 第 3 步:彻底分离(由 SDK 守护进程独占设备)—— 只有当出现第二个设备消费者时
  才值得。
* 动机依然是实测的:失效句柄让 `Device_Close` 在 `libhtraapi` 内段错误,导致整个 worker
  丢失(8 个 core,每次重启一个)。

## 5. 本轮有意不做的零散项

* **[待做] [受阻]** 给系统 core dump 设上限(`/etc/systemd/coredump.conf` 的 `MaxUse` +
  `coredumpctl vacuum`);需要 root。当前约 1.6 GB。注意 `ulimit -c 0` 在这里**挡不住**
  core —— 已实测。
* **[待做]** 去掉前端那条冗余的 1.1 Hz `/api/state` 轮询:WebSocket 已经在推同一份 payload。
  它是日志量的唯一来源(开着标签时 16.5 MB/天)。滚动已经把它限制住了,所以这是整洁问题,
  不是紧迫问题。
* **[待做]** 绝对电平可溯源:tinySA 没有校准过的绝对输出,所以"0.4 dB 以内"指的是通路一致性。
  有校准源才能收口。

## 6. 未验证项(保持标注)

* `Bus` 以外的 `IQS_TriggerSource`(Level / External / GNSS1PPS / Timer / SpectrumMask)。
* 超过 2^24 的 `TriggerLength` 与背靠背连续帧(触发到触发的间隔)。
* 经 `DSP_DDC` 的捕获(所有探针都读原始 IQS 流)。
* 超过 40 s soak 的长期稳定性与温漂。
* `QDCAutoMode` 对镜像抑制的影响(QDC 保持 off,即生产默认)。
* Tier2 在**真实**调制信号上的表现(本台面无 PSK/QAM 源)。

## 7. 实现必须遵守的实测约束

| 约束 | 实测值 |
|---|---|
| 配置后丢弃 | 0.25 s;不丢弃时 10824/10824 次取数失败 |
| 重配引发的 `-9` 卡死 | 6 次速率切换中 2 次;恢复逻辑必须复用而非另写 |
| `RefLevel_dBm` 是输入增益 | RefLevel 0 时线性到约 −20 dBm;−15 dBm 时低读 3.9 dB |
| `ScaleToV` 跨 RefLevel | 30 dB 设置对应 33.7 倍 —— 已是绝对伏特,无需额外因子 |
| 深捕获深度 | 2^24 点、零丢包、取数约等于实时 |
| **`FixedPoints` 取数配方** | 触发后**逐包连续读、读前不做任何取数**:连续读 3/3 次拿满 2^24 点(比值 1.00);帧中途插一次读会精确丢一包(114832/131072,并耗 20 s 的 `-10`);在稳定窗内读则直接毁帧(0 点、永久 `-10`)。Bus 触发启动本身就是冲队列,所以抓帧不丢弃 |
| 抓帧深度粒度 | `PacketCount` = ceil(深度 / `PacketSamples`):131072 点为 9 包,2^24 为 1034 包 —— 每一次运行里计划包数与实到包数都一致 |
| 抓帧取数比值(经服务端) | 2^24 点为信号时长的 1.22 倍;131072 点为 1.92 倍(固定 settle/装帧开销主导短帧) |
| Tier1 绝对电平(经服务端,五种测量) | tinySA CW −25.0 dBm → 均值 −25.00 dBm(经帧通路在 120 MHz 复测)、单音电平 −25.2 dBm、峰值 bin −28.3 dBm(窗 ENBW 2.00 bin)、噪声密度 −138.9 dBm/Hz |
| VSA 帧通路 | 深度 2^17/抽取 16 时 3 s 内 6 个 RTAF + 6 个 VSAD;最新 VSAD 仅靠 NumPy 可解;2 s 停顿后仍只挂最新点云 |
| 产物与探针对账 | FINDINGS 第 15 节:−25 dBm 源 → 均值 −25.23/单音 −25.2 dBm;2^24 点 0 错误、1.41 倍信号时长;Tier2 定时 8–30 dB 内 ≤0.026 样本;命令 33/33 通过;未验证清单与第 6 节一致 |
| 载波上的盲符号率 | CW 单音没有符号谱线:估计给出任意的 554.95 kHz,因此 Tier1 绝不把它当测量值呈现(Phase 2 必须拒绝) |
| 流式开销 | 纯取数占单核 1.5–18 %;Welch 25 %;频谱图 120 % |
| 盲符号率 | 需要 sps ≥ 4;sps 2 时估计崩溃 |
| 载波相位 | M 次方估计器固有四重模糊 |
| 镜像抑制 | 78–92 dB —— 不是限制因素 |
| DC 频点 | 0.2–1.6 dB,随驱动电平变化(本振相干泄漏,不是陷波) |
