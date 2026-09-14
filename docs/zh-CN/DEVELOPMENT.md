# 开发指南（v1.5.6）

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
| 运行态（连接状态、触发 armed/hit、当前模式） | `core/store.ts` | 单一写者也要走 setter |
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

纯函数放 `dsp/`（或后端 `demod/`），**先写单测再接入**；不要在渲染循环里做重计算；
DLL 调用一律在设备锁下、放到 `to_thread`，并确保有看门狗（见 §7）。

---

## 6. 测试策略：在哪一层断言什么

| 层 | 工具 | 断言什么 | 反例（不要这样写） |
|---|---|---|---|
| 纯逻辑 | vitest / pytest | 算法、状态机、契约（i18n parity、帧 fixture、schema、槽位语义） | — |
| 会话/设备边界 | pytest + stub 设备 | 结果组装、策略、错误路径（**不需要厂商库**） | 为了测试去连真机 |
| 协议 | 两端 golden fixture | 字节布局 | 只在一边测 |
| **端到端** | Playwright（真机或假后端） | **用户可见结果**：画布像素、DOM 文本、真实点击后的设备状态 | `dataset.rtaFrames`（只说明帧交给了渲染器，不代表画出来了） |
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
  `noise/peak/nEma/pEma/range/ref/applied/shown`）；SDR 音频状态在 `dataset.sdrAudio`。
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
| 干净环境 `./test.sh` 失败 | 依赖声明不完整 | 运行时/开发/锁定三份依赖文件 | CI 在干净环境安装 |

---

## 13. 日常命令

```bash
make ci                      # 全部无硬件门禁（测试/静态检查/契约/守卫/构建）
make run | make stop         # 启停服务（supervisor + worker）
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
