# 开发指南（v1.7.2）

> **这份指南不是基准规范，而是一份会持续改进的工作约定。** 它记录了本项目在
> 2026-09 的架构评估与重构中真实踩过的坑，以及从那之后固定下来的做法。
> **改进方式：每当有一个缺陷逃过测试，就补一条"教训 + 规则 + 守卫（测试/脚本）"**，
> 见 §12 教训台账。规则可以因为更好的做法被替换，但**每一条规则都应该有对应的守卫**，
> 否则它只是一句口号。

---

## 1. 怎么用这份指南

三个时刻查：

1. **动手前**：在 §2 架构图里确定"这段代码属于哪一层"，再决定它应该放在哪里；需要新扩展点时看 §4。
2. **写代码时**：§3 状态归属、§5 增加功能清单、§7 性能规则。
3. **提交前**：§6 测试断言该怎么写、§9 提交与版本、§10 守卫清单跑一遍（`make ci`）。

最短路径：`make ci`（无硬件全绿）→ 有硬件时 `make hw-test` → 动过数据面再 `make bench`。

---

## 2. 架构与代码归属

### 2.1 后端（`web_sa/`）

| 模块 | 职责 | 不该做的事 |
|---|---|---|
| `hardware/sdk_bindings.py` | **唯一**直接接触 `libhtraapi` 的地方（含手写的 PNM/ADM/IQStream 结构体） | 业务逻辑；其它模块禁止 `import htra_api`（有守卫） |
| `hardware/state.py` | 设备状态：按所有者分组的 `Swp/Rta/Sdr/Trigger` 参数 + 共享前端/meta 字段；分组字段同时保留扁平别名（过渡期） | 业务逻辑；新代码应使用分组（`state.rta.span_hz`） |
| `hardware/device.py` | 设备生命周期、缓冲区、一次 `step()`、会话宿主 | 控制环决策、协议编码 |
| `hardware/auto_reference.py` | Auto Ref 控制环（纯决策，可单测） | 直接调 DLL |
| `measurements/base.py` | 会话接口：`enter/exit/step` + 采集策略（`acquisition_timeout/pacing/dedupe_freq/reconfigure`）+ `is_ready/request_stop` + `health` | 感知具体模式名 |
| `measurements/{harmonic,phase_noise,rta,sdr}.py` | 各模式会话：配置设备、产生帧 | 处理 HTTP/WS |
| `measurements/framer.py` | **唯一**的帧编码（FREQ/POWR/RTAF/AUDF） | 依赖硬件 |
| `measurements/results.py` | 纯结果载荷构造（可单测） | 调 DLL |
| `web/commands.py` | 声明式命令表：`CommandSpec` + `ParamSpec` + 守卫标志 | 传输细节 |
| `web/ws.py` / `web/http_api.py` | 传输适配（WebSocket / REST + STATUS 序列化） | 业务分支 |
| `web/publisher.py` | 采集调度：一次 step、背压、STATUS 推送 | 按模式名分支（策略在会话里） |
| `web/client_stream.py` | 每客户端单一写者、latest-wins、`FRAME_POLICY` | 逐帧解析 |
| `web/recovery.py` / `supervisor.py` | `fatal()`/`EXIT_FATAL` 与进程级重启 | 业务判断 |
| `web/jsonutil.py` | JSON 边界：NaN/Inf → null、单次序列化 | — |

### 2.2 前端（`frontend/modern/src/`）

| 目录 | 职责 | 备注 |
|---|---|---|
| `core/model.ts` | 共享类型（叶子） | 不 import 任何应用模块 |
| `core/params.ts` | 参数槽位（`confirmed/desired/epoch` + TTL + 持久化） | 参数唯一所有者机制 |
| `core/results.ts` | 测量/显示**结果**数据 + 显式 setter | 没有 desired/confirmed 语义 |
| `core/store.ts` | 运行态（连接、触发运行态、当前模式…） | 参数与结果都已移出，只 re-export |
| `core/{ws,frames,wsSend}.ts` | 协议：JSON 命令 + 二进制帧解码（`frames.ts` 是唯一帧定义） | 新帧型见 §5.4 |
| `core/{units,level,frequency,fmt,i18n,theme,markerCommon}.ts` | 纯工具/数据 | 叶子 |
| `dsp/` | 纯函数 DSP（平滑、寻峰、限制线、电平交叉、统计…） | **必须有单测** |
| `render/` | 绘制：`spectrum.ts`（扫频/RTA 视图枢纽）、`registry.ts`（视图注册）、`redraw.ts`（重绘 seam）、`plot.ts`（几何）、各类 canvas 层 | 只画，不改状态 |
| `ui/` | 交互与状态：`*State.ts`（槽位）、`panels/*`（面板动作）、`controls.ts`（绑定/编排）、`measure.ts`（测量状态机）、`measureRegistry.ts`（页签注册） | 尽量薄 |
| `meas/` | 各测量：结果处理 + 叠加绘制 + 页签注册 | 通过 `measureRegistry` 注册 |
| `audio/` | SDR 音频：worker + worklet + 重采样 | 不阻塞主线程 |

### 2.3 依赖方向

只允许"上层 → 叶子"。`madge --circular` 由守卫强制为 **0**；`render/registry.ts`、`render/redraw.ts`、
`core/params.ts`、`core/model.ts`、`core/i18n/` 都是叶子，**永远不要**从它们 import 应用模块（会立刻形成环）。

---

## 3. 状态归属（最重要的一条）

| 类别 | 放哪 | 规则 |
|---|---|---|
| **参数**（用户设定 + 后端确认） | `ui/*State.ts` 的槽位 | 只有 STATUS 处理器调 `confirm()`；只有用户动作调 `set()`；读取一律 `get()` |
| 客户端独有偏好（后端从不上报，如显示单位、瀑布范围、音频开关） | 槽位 + `authoritative: true` | 不写 `authoritative` 会**在 TTL 到期后回退**（真实踩过） |
| **结果/数据**（迹线、密度、峰值表、测量结果） | `core/results.ts` | 显式 setter，没有 desired/confirmed |
| 运行态（连接状态、触发已设置/已命中、当前模式） | `core/store.ts` | 单一写者也要走 setter |
| 持久化 | 槽位的 `persistKey` | 只允许一处读写 localStorage |

**热路径规则**：不要在每帧数万次的循环里调槽位 `get()`（它带 `Date.now()` 与 pending 检查）。
密度累加内层循环曾因此每帧调用约 60 万次，把主线程拖满 → STATUS 延迟 → 模式按钮基于过期值切换。
**循环外读一次**。

**这条规则是为了根治六类真实发生过的失效**（来自参数状态排查，均已修复并固化）：
① 意图期取值、确认期使用（点了 preset 又立刻进 SDR，交接到的中心还是旧值）；
② 私有缓存与渲染值失同步后永久锁死（缓存认为"已应用"，差值小于阈值 → 拒绝纠正）；
③ 全局复位不完整（preset 没有作废"上次操作记忆"→ 旧意图复活）；
④ 同一参数多写者（`displayRef` 曾有 7 个写入者，无所有权约定）；
⑤ 持久化不闭环（偏好只读不写，刷新即丢）；
⑥ 用哨兵值表达"无值"（如 0 Hz 既是合法频率又是"未设置"）。
新增状态时对照这六条自检。

---

## 4. 扩展点与注册

- **注册必须可达**：模块级注册（`setRenderer` / `registerViewRenderer` / `registerMeasurementTab`）
  只有在模块被 import 时才生效。破坏循环依赖后"没人 import 它"本身就是故障
  （曾导致画布全空且无任何报错）。入口 `main.ts` 用**副作用 import** 拉入这些模块，
  `python3 tools/check_registrations.py` 由 CI 强制。
- **一个显示位只能有一个所有者**：瀑布图与测量结果表抢同一个位置，测量激活时必须关闭并禁用瀑布，
  结束后恢复用户原选择。
- **跨模块字符串标识必须成对解析**：单位组的键是 `rta_center`、输入框 id 是 `input-rta-center`，
  两者不一致曾让单位按钮/虚拟键盘双双静默失效。统一走 `inputForField()` / `fieldForInput()`。
- **模式策略属于会话**，不属于调度层：超时/节流/去重/重配都是会话的方法，新增模式不应修改
  `publisher.py`/`ws.py`（守卫会检查 `mode ==` 分支数）。

---

## 5. 增加一个功能（清单）

### 5.1 新增一条命令或参数（后端）

1. `web/commands.py`：加一行 `_REGISTRY` 条目 + 在 `PARAMS` 里用 `ParamSpec` 声明每个字段
   （类型/边界/单位/默认/条件必填），交叉字段规则放 `EXTRA_VALIDATORS`；
2. 边界：能用能力表就用 `caps`（不要写字面量，守卫会数 `maximum=<数字>`）；
3. 模式/会话限制用表标志（`SWP_OWNED` / `SWP_ONLY` / `NOT_IN_SDR`），不要写 `if mode == ...`；
4. 测试：表一致性测试 + `tools/command_sweep.py`（真机）会覆盖新命令；
5. 前端：参数用槽位（§3），命令用 `send({cmd: ...})`，`/api/schema` 会带上新字段。

### 5.2 新增设备型号/能力

改 `DeviceCapabilities.from_model` 一行（频段 + 限值），**不要**在命令校验里写死型号相关数字。
`pnm_supported` 这类特例应逐步换成 `supports('pnm')` 之类的能力查询。

### 5.3 新增测量模式（会话）

1. 后端：`measurements/<name>.py` 继承 `MeasurementSession`，实现 `enter/exit/step`，
   以及需要的策略方法（`acquisition_timeout`、`pacing`、`dedupe_freq`、`reconfigure`、`health`、
   `request_stop/is_ready`）；在 `measurements/__init__.py` 的 `_SESSIONS` 注册（惰性 import）；
   结果载荷放到 `results.py` 之类的纯模块里，便于无硬件测试；
2. 前端：`render/registry.ts` 注册视图渲染器；若有页签，`registerMeasurementTab`；
3. 测试：stub 设备单测（不调 DLL）+ e2e 断言"进入后画布真的画出来了"；
4. 若与其他显示元素（瀑布、限制线、图表）冲突，先确定**唯一所有者**再动手。

### 5.4 新增帧类型

`measurements/framer.py` 加编码器 → `web/client_stream.py` 的 `FRAME_POLICY` 加保留策略
（未知类型默认 latest-wins）→ `core/frames.ts` 加解码 → `tools/gen_frame_fixtures.py` 生成 golden
fixture → 两侧测试各断言一次（Python 断言 fixture 与编码器一致，TS 断言解码结果与 manifest 一致）。

### 5.5 新增面板/界面文案

1. `ui/panels/<name>.ts`（面板动作）+ `data-action` 绑定；不要往 `controls.ts` 里堆动作；
2. i18n：按命名空间加到 `core/i18n/dict.<域>.ts`（**en 与 zh 都要加**，parity 测试会强制）；
3. 新元素 id：`tools/check_dom_ids.py` 会检查"TS 读取但 index.html 不存在"；
4. 用户可见行为要加 e2e 断言（§6）。

### 5.6 新增 DSP/SDR 处理块

纯函数放 `dsp/`（或后端 `demod/`），**先写单测再接上**；渲染循环内不做重计算；每个 DLL 调用都在设备锁下
跑在 `to_thread` worker 上并有看门狗（见 §7）。

### 5.7 交互规则：「自动」类按钮是一次性动作，且不得静默

凡是结果要付一次器件重配代价的操作（Auto Scale、拟合、校准），都是**瞬时动作 + 可见反馈**，不是跟踪开关：

1. 一次点击只产生一个决策——算一次、应用一次、结束；
2. 已经处于良好状态时必须零代价（不重配、不跳变）；
3. 按钮要显示"正在做"（由后端 `adjusting` 驱动发光/busy，并有客户端兜底计时器），完成后说明结果
   （`applied`/`ok`/`no_data`），拒绝也绝不静默。放置不会因无信号而拒绝：它锚定噪声底，
   「没有信号」只是一个待放置的电平，不是不动作的理由；
4. 动作进行中**不锁定**任何相关控件——一个把用户锁在它正在调整的那个字段之外的"模式"，读起来就是 bug
   （旧的跟踪式 `Auto` 正是如此）；
5. 后台**安全纠偏**是另一件事：始终设置并启用、限速运行、且只做**保护方向**（IF 过载、严重削顶），
   不需要用户去打开，也绝不反转用户刚用过的控件。唯一的例外是设置变更（span/中心/RBW/窗函数/抽取率）：
   它为新几何预备**恰好一次**重拟合（首次决策即撤销，因此不可能退化为跟踪），而用户手输的电平仍然是他的；
6. **模式的用户设置就是偏好**：用 `persistKey` 持久化，并在进入该模式时重新应用。切换模式不是复位——
   SDR 之前每次进入都从扫频视图重推频率、捕获带宽与解调模式，把用户留在那里的设置静默丢弃。交接靠显式
   手势（Shift+点击 / 频段预置），只有首次使用才推导默认值；
7. **器件界限只声明一次、处处读取**：Ref 范围、RTA span/点数、触发电平范围等，校验、SDK profile、
   自动参考环与客户端都读能力行（STATUS 的 `caps` / `config.ref_bounds`），不各留一份——同一机型
   曾被三套字面量各自夹过（排查硬编码器件值时发现）；
8. **拒绝类消息**要在「理由不再成立」的那一刻撤下，而不是等计时器走完：「等待迹线」在第一帧到来时结束，
   而记录了决策依据（峰值/噪底）的提示会在该观测发生变化时撤下。对测量下结论、却在测量变了之后还留在屏幕上，
   正是提示变成噪音的方式（用户报告：迹线都画出来了，消息还挂满 6 秒）。「无信号」拒绝已无产生者
   （见 §5.7.3），这条路径只为取值表兼容而保留，不再对应真实决策。

---

## 6. 测试策略：在哪一层断言什么

| 层 | 工具 | 断言什么 | 反例（不要这样写） |
|---|---|---|---|
| 纯逻辑 | vitest / pytest | 算法、状态机、契约（i18n parity、帧 fixture、schema、槽位语义） | — |
| 会话/设备边界 | pytest + stub 设备 | 结果组装、策略、错误路径（**不需要厂商库**） | 为了测试去连真机 |
| 协议 | 两端 golden fixture | 字节布局 | 只在一边测 |
| **端到端（无硬件）** | `make e2e-fake`：`ui_smoke.py`（渲染与接线，29 项）+ `state_regression.py`（参数状态机契约，72 项），同一假服务，CI 运行 | 画布像素、控件到达后端、模式切换/页签/瀑布/i18n/键盘、峰值表使用槽位门限；槽位/在途/交接/Preset/刷新/快速连切；一次性 Auto Scale（发光 → 一步落定 → `ok` 且不重配）；设置变更只重拟合一次且绝不撤销手动电平 | 只断言 dataset/计数器；**为让假后端通过而放宽断言**（会同时削弱真机轮次；器件相关检查用 `require_device=True` 显式跳过） |
| **端到端（真机）** | Playwright + 真机 | **用户可见结果**：画布像素、DOM 文本、真实点击后的设备状态 | `dataset.rtaFrames`（只说明帧交给了渲染器，不代表画出来了） |
| 性能 | `tools/bench.py` + 基线 | 帧率/切换延迟/CPU 的**可比**数值 | 在设备告警或残留负载下比较 |
| 硬件冒烟 | `tools/hardware_smoke.py` + tinySA | 真实信号下的电平/帧完整性 | — |

两条硬规则：

1. **"功能死掉时不会失败的测试，都不是测试"**——先想"如果这段代码完全没执行，这个断言会失败吗？"
2. **断言代理量要显式说明**：如果因为环境限制只能断言代理量（例如没有真机），在测试里写清它代理什么、
   以及真正的用户可见断言在哪一层补。

---

## 7. 性能与并发规则

- **先测量再优化**：`make bench`（固定配置、干净设备状态、单客户端）与 `tools/bench_baseline.json`；
  单次采样可能因机器噪声误报，bench 会在判定回归前复测一次。
- **后端**：所有 DLL 调用在设备可重入锁下串行；慢调用走 `asyncio.to_thread` + 看门狗；
  超时/致命错误走 `fatal()`（进程级恢复由 supervisor 负责），不要吞异常。
- **不要阻塞事件循环**：publisher 的采集在 worker 线程；HTTP/WS 必须保持响应。
- **背压**：每客户端单一写者；高频帧 latest-wins；`FREQ` 单独保留；音频走 FIFO 且 `seq=0` 清空陈旧数据。
- **前端热路径**：循环外读槽位；避免每帧分配大对象；渲染节流（RTA ~30fps、SWP ~30fps）。

---

## 8. 失败处理与可观测性

- `fatal(reason)` 是唯一的致命退出路径（`EXIT_FATAL` → supervisor 重启）；错误码是跨进程契约。
- **STATUS 是唯一的诊断面**：需要给 UI/脚本看的东西写进 STATUS（用 `health()`/`auto_reference_view()` 之类
  的显式视图方法），不要靠日志。
- 画布右上角的状态栈是共享区域：所有指示器 `pushStatus` 叠加；**状态变化要主动 `requestRender()`**
  （IF 溢出时设备不发帧，只靠帧循环绘制就会永远看不到告警）。
- 日志：用 `logging`，在边界处 `log.exception`；不要 `print`（`main.py` 的启动横幅除外）。

---

## 9. 提交、版本与发布

- **提交信息**：英文、`type(scope): summary`（feat/fix/refactor/test/docs/chore/perf），说明"删掉了什么/
  为什么"，而不是"改了哪些文件"。
- **每个提交都能通过测试**：这是可二分（bisect）的前提。提交前跑 `make ci`；
  改动跨多个提交时，可用
  `git worktree add /tmp/wt <commit> && (cd /tmp/wt && python3 -m pytest tests/ -q)` 逐个验证
  （曾发现"测试先于实现"的提交，历史已重建修正）。
- **版本单一来源**：改 `pyproject.toml` → `python3 tools/sync_version.py`（同步 `package.json` 与
  `index.html`）；`--check` 在 CI 中。
- **发布**：bump 版本 → 构建前端（**服务提供的是 `dist/`，忘记 build 等于测旧包**）→ `make hw-test` →
  `git merge --no-ff` 到 master → tag → push。
- **分支**：`feature/*`、`refactor/*`、`analysis/*`；文档与代码分开提交，便于审阅。

---

## 10. 守卫清单（CI 强制什么，以及怎么改基线）

| 守卫 | 命令 | 基线文件 |
|---|---|---|
| 后端测试 | `python3 -m pytest tests/ -q` | — |
| 静态检查 | `python3 -m ruff check web_sa tests tools` | — |
| 前端类型/测试 | `npx tsc --noEmit` / `npm test` | — |
| 版本一致 | `python3 tools/sync_version.py --check` | — |
| 帧 fixture 与编码器一致 | `python3 tools/gen_frame_fixtures.py --check` | `tests/fixtures/frames/` |
| DOM id 契约 | `python3 tools/check_dom_ids.py` | — |
| 双语文档结构一致 | `python3 tools/check_docs_parity.py` | — |
| 注册点可达 | `python3 tools/check_registrations.py` | — |
| 架构指标不退化 | `python3 tools/quality/architecture_guard.py` | `tools/quality/baseline.json` |
| 性能不退化 | `python3 tools/bench.py --check tools/bench_baseline.json` | `tools/bench_baseline.json` |

**基线只降不升**：改进后执行 `--update` 收紧基线；确需放宽时，必须在该提交的说明里写清原因
（例如上游依赖变更），否则后来者无法判断是"有意放宽"还是"悄悄退化"。

---

## 11. 排障手册（按症状）

| 症状 | 先查 | 常见原因 |
|---|---|---|
| 服务起不来 / 设备被占用 | `pgrep -af web_sa`、`fuser -v /dev/ttyACM0`、`tail /tmp/websa.log` | 上一个探针进程没退出；必须"一个进程拥有设备" |
| **画布全空、无任何报错** | 画布像素数（e2e 的 `painted_pixels`）、`check_registrations.py` | 渲染器未注册（模块没被 import）；`dist/` 未重新构建 |
| 模式切不动 / 要按两次 | STATUS 是否在到达（后端 1 Hz 推送）、是否有 pending 意图 | 主线程被拖满（热路径 `get()`）；pending TTL 未到期 |
| 采集超时 / worker 反复重启 | `tail -50 /tmp/websa.log`（含 `faulthandler` 栈）、supervisor 退出码 | DLL 卡死或原生崩溃；超时看门狗按扫描时间缩放 |
| UI 值与设备不一致 | STATUS 的 `req`（请求）与 `actual`（实际） | 槽位 `desired` 未确认（命令被拒/在途）；把 STATUS 当唯一事实 |
| 单位按钮/虚拟键盘不生效 | `fieldForInput(input)` 是否返回字段名 | 字段键与输入框 id 拼写不一致 |
| 告警/提示只在某瞬间可见 | 是否有独立的重绘触发 | 只依赖帧循环绘制，而该状态恰好没有帧 |
| 前端改了没生效 | `npm run build` 是否执行、浏览器是否刷新 | 服务提供 `dist/`，不重新构建就还是旧包 |

- 前端**诊断键**（排查 auto-ref/缩放类问题直接读它们，不必加日志）：
  `#spectrum.dataset.sdrRef`（已应用值）与 `dataset.sdrRefDbg`（auto-ref 全部内部量：
  `noise/peak/range/ref/applied/before/shown`——`before`/`shown` 让"决策即用户所见"可从外部断言，
  且每次决策都会记录，因此"本来就无需改动"也可见）；STATUS 里的 `auto_ref.{result,target,seq,adjusting}`
  是同一件事的后端一半（`seq` 用于区分新答复与残留的旧答复）；SDR 音频状态在 `dataset.sdrAudio`。
- 后端：`WEBSA_TRACE=1 ./run.sh` → `grep '\[trace\]' /tmp/websa.log`；原生崩溃会打印全线程栈
  （`faulthandler`），supervisor 的退出码/重启记录也在同一份日志里。

---

## 12. 教训台账（每条都对应一个守卫）

| 现象 | 根因 | 规则 | 守卫 |
|---|---|---|---|
| 评估报告称"缺 77 个 i18n 键"，实际只缺 1 个 | 支撑结论的正则漏掉含 `-` 的键、在嵌套花括号处截断 | 支撑结论的脚本必须先在**已知答案**的样本上自检 | i18n parity 测试；报告 §8.1 更正 |
| `controls.ts` 1548 行、14 条循环依赖 | 目录分层 ≠ 扩展点；枢纽模块被所有人反向依赖 | 用 seam + 注册表反转移赖；扩展点见 §4 | `madge` 基线、面板拆分 |
| 参数"多个写者、互相覆盖" | 缺少单一所有者 | 参数用槽位（`confirm/set/get`），结果用仓库 | `params.test.ts`、`status.test.ts` |
| **画布全空**（用户报"无法使用"） | 破环后枢纽模块没人 import，注册副作用未执行；测试只断言代理量 | 注册副作用必须由入口 import；测试断言用户可见结果 | `check_registrations.py`、e2e 像素断言、`entryWiring.test.ts` |
| RTA 单位按钮/键盘失效 | 字段键 `rta_center` 与 id `input-rta-center` 不一致 | 跨模块标识统一解析 | `units.test.ts`、e2e 6c |
| 测量时瀑布图仍占位 | 一个显示位两个所有者 | 显示位单一所有者 + 禁用并恢复 | e2e 7b、`status.test` 用例 |
| Ref 溢出告警看不到 | 只在帧循环里绘制，而溢出时没有帧 | 状态变化主动重绘 | `status.test` 用例 |
| bench 误报 2.5×/1.9× CPU 回归 | 残留设备配置/信号、单次采样噪声 | 固定配置、干净状态、复测一次再判定 | bench 确定性 + `device_warning` 记录 |
| "测试先于实现"的提交 | 提交时未从干净树跑测试 | 每个提交都必须能通过测试 | worktree 逐提交验证流程 |
| Auto 提示"已调整"，但画布与 Ref 输入框仍是旧值 | 客户端有个"现在归用户所有"的标志，挡住了自动纠偏去改显示，于是只有器件电平变了 | 自动纠偏必须作用到用户能看到的一切（画布 + 输入框 + 提示），否则就不要播报；保护手动值要用"是否是新决策"来判定，而不是拒绝跟随纠偏 | e2e 9a2（画布 + 输入框）、`refAutoScale.test.ts`（跟随自动纠偏；残留目标不重复应用） |
| Auto 之后 Ref 输入框显示旧值 | SDR 里输入框在后端电平写完之后又被客户端显示 ref 覆盖；dB（相对）模式下还被钉成 `0` | 每个显示值只有一个所有者：输入框显示 dBm 参考电平（就是 Set 下发的值）；相对刻度顶端只是渲染细节 | e2e 9a2、`status.test.ts` |
| Auto 的提示把 Ref 按钮挤来挤去 | 提示被写进了它所描述的那一行；移到组头只是把问题挪了个位置 | 瞬态反馈进画布状态栈（无宽度限制，且紧邻它所回应的条件）；一个显示值只有一个所有者 | e2e `ui_smoke` 2a（`dataset.notice`、行 box 不变）、`refAutoScale.test.ts`（提示 TTL/世代） |
| 按 Ref 上箭头后 Auto 把迹线拉回去 | 量程纠正了"噪声底低于下沿"这个方向——那是显示选择，不是故障，等于撤销用户刚按的按钮 | 自动纠偏只做**保护方向**（器件过载、严重削顶），绝不反转用户刚用过的控件；否则只报告，让显式动作去修 | e2e `state_regression` 9c、`test_auto_reference.py`、`test_device_state.py` |
| SDR 里 `AUTO_SCALE` 被静默拒绝（显示在 -60 dBm） | `current_ref` 是**显示值**，却按器件 Ref 范围校验 | 按"值的所有者"的范围校验，而不是按它最终影响的器件范围 | `test_ws_commands.py::test_auto_scale_accepts_a_display_ref_outside_the_device_ref_range` |
| **「迹线不在画布中，或只有极小部分落在下沿以下」**：小 span、无外部信号，按 Auto 却报 `no_signal` 且不动作 | 拟合仍带着以**峰值**为锚点时代的信号门限（`峰值 - 噪底 < 15 dB -> no_signal`）。以噪声底为锚点本不需要信号，这道门限恰好拒绝了噪底锚点能处理的场景——而 `inside` 分类把它藏住了（比下沿低 0-3 dB 也算 inside） | 自动类动作必须按它真正的锚点行事：锚点是噪底时，「没有信号」只是一种普通放置情形（无需变更则 `ok`，否则给出电平）。分类器先说 `inside` 又拒绝动作，是自相矛盾 | `test_auto_reference.py`（仅噪声且贴/越下沿、三个上报设置都最终入窗），e2e `state_regression` 9e |
| 设置变更（span / 捕获带宽）后迹线仍在下沿以下，直到按下 Auto；而 SDR 拟合即使窗口能到更低也无法低于器件 Ref 范围 | 放置只在被请求时运行，新几何继承了旧电平；且 SDR 的 target 是**显示**电平，却按**器件**行夹取 | 设置变更使原放置失效：为新几何预备**恰好一次**重拟合（绝不退化为跟踪，限速，首次决策即撤销），而用户手输的电平仍然是他的；并且每个值按它**所有者**的范围夹取——显示 target 按显示范围，IQS 写入按器件范围 | `test_auto_reference.py`（每次几何变更只重拟合一次、手动电平不被推翻、SDR 拟合跟随显示范围而 IQS 写入不越界），e2e 9e，`refAutoScale.test.ts`（未经按下的 SDR 决策也会移动显示） |
| Level offset 移动了迹线却没移动图上的幅值数 | 同一量由两层绘制：`getY()` 平移迹线，而 y 轴刻度与画布 marker 读数打印的是原始器件 dBm | 一个量画在多处时只能在一处换算（`fmtAxisLevel`/`fmtReadoutLevel`）；坐标轴属于显示域，不属于器件域 | `peakThr.test.ts`（读数规则）、e2e `ui_smoke` 2a2（`dataset.yLabels` 跟随 offset） |
| 信号出现后「No signal to fit」还挂在屏幕上 | 拒绝消息用的是固定 6 秒计时，只有*新决策*才会替换它 | 拒绝是对测量下的结论：记下它依据的观测，观测一变（或第一帧到来）就撤下，别依赖计时 | `refAutoScale.test.ts`（观测变化/第一帧即撤下）、真机探针 |
| Ref 30 dBm 无声地变成 27 | 器件按自身上限（取决于它选的衰减档）夹住并回显；UI 静默跟随回显值 | 设备回显与请求不一致时要说出来（`req` vs `actual`）——与「拒绝也要播报」同一条规则 | `status.test.ts`（限制提示，同组只报一次）、FAQ |
| Ref 范围 / RTA span 在三个地方各写一份字面量 | 能力行已经声明了它们，但 `device.py` 的 profile 夹取、自动参考环、客户端各留了自己的一份 | 器件界限只发布一次（STATUS 的 `caps`、`config.ref_bounds`），处处读取；测试断言「环与夹取都跟随能力行」 | `test_config.py`（`ref_bounds`）、`test_auto_reference.py`（目标夹取跟随 caps）、`test_device_state.py`（caps 负载） |
| 测试用单一样例断言器件值（"30 -> 27"） | 把测试数据当成了被测性质：消息应当跟随**回读值** | 断言**关系**（多组样例，或一个会变化的取值），不要断言一次记录下来的数字；硬编码实现必须无法通过 | `status.test.ts`（数据驱动多组 + 「限值变化时跟着变」） |
| 每次进入 SDR 都从扫频视图重新推导设置 | 模式入口把"切模式"当成重新开始（频率取扫频中心、解调按频段猜、decimate 写死 16），于是用户自己的调谐与收听设置被丢弃 | 模式的用户设置就是**偏好**：持久化并在进入时重新应用；交接频率靠显式手势（Shift+点击 / 频段预置），只有首次使用才推导默认值 | e2e `state_regression` 5（往返保持 + Shift+点击仍可交接）、`status.test.ts`（音频偏好保持）、FAQ |
| Shift+点击进入 SDR 落在了错误频率 | `listenAtFreq` 只设置了捕获中心，旧的解调频率留着，后端为追它把捕获中心搬走（实测：点 216 MHz 落到 987 MHz） | 「听这里」= **同时**设置捕获中心与解调频率；只设置一对中的一半，等于把 bug 留给另一半 | e2e `state_regression` 5（`Shift+click hands that frequency to SDR`） |
| 干净环境 `./test.sh` 失败 | 依赖声明不完整 | 运行时/开发/锁定三份依赖文件 | CI 在干净环境安装 |
| 拔出频谱仪后画面定格，但 STATUS 仍报 `connected: true`，重新接入也不恢复 | 从未检测断开：采集路径把总线错误当成“没取到帧”，只有 native 崩溃/超时才惊动 supervisor | 传输故障是前端必须看到的状态：在没有原地恢复能力的路径（扫描）上，连续总线错误置 `connected=false`，调度器停止步进死句柄，worker 链路循环重开设备 | `test_link_recovery.py`（链路循环恢复会话；连续 -8 翻转 `connected`）、`test_publisher.py`（断开时不做采集）、`status.test.ts`（断开告警 + 重绘） |
| 真机 RTA 在 `SET_FREQ` 后出现一连串 `-9`，链路看门狗误判为拔线并关闭设备 | 扫描模式命令在 RTA 会话持有设备时下发了 `SWP_Configuration`，把设备从 RTA 切走（与 Preset 在 SDR 下的同一类 bug）；可自恢复的错误串随后被当成了拔线 | 命令只能重配拥有设备的那个模式：RTA 下扫描参数只作为存储偏好（不下发 `SWP_Configuration`）；可自恢复的错误串不得升级为链路丢失——链路检测只放在没有原地恢复能力的路径 | `test_ws_commands.py`（RTA 下 SET_FREQ/SET_DETECTOR 绝不调 `configure_swp`）、真机 `state_regression` + `hw-test` |

---

## 13. 日常命令

```bash
make ci                      # 全部无硬件门禁（测试/静态检查/契约/守卫/构建）
make run | make stop         # 启停服务（supervisor + worker）
make restart | make status   # 重启服务 / PID、运行时长、内存、CPU、日志路径与大小、实时链路
make e2e-fake                # 无硬件：假后端上跑 ui_smoke（29 项）+ state_regression（72 项），CI 同款
make hw-test                 # 真机：tinySA 冒烟 + 24 命令扫描 + UI 状态机回归（45 项）
make bench                   # 与基线比较帧率/切换延迟/CPU
python3 tools/bench.py --write-baseline tools/bench_baseline.json   # 重录基线（先确认干净状态）
python3 tools/command_sweep.py        # 单独跑命令层契约（真机）
python3 tools/e2e/state_regression.py # 单独跑 UI 状态机回归（真机）
python3 tools/check_registrations.py  # 注册点可达性
python3 tools/quality/architecture_guard.py --baseline   # 查看当前架构指标
```

---

## 14. 已知未完成（指针）

见 `ARCH_REVIEW.md` §9.3：假后端 + e2e 进 CI（当前最高价值）、e2e 覆盖 Firefox、`DeviceState` 按模式拆分、
`controls.ts` 剩余无用导出、ESLint（受上游 `typescript-eslint` 与 TypeScript 7 的兼容性阻塞）、
根目录临时 TODO 归档、按命令裁剪 STATUS。**本指南与那份清单一起演进**：完成一项就更新两处。
