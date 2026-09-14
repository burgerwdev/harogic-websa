# 架构与代码评估报告（v1.5.5）

> 分支：`analysis/arch-review`（自 `master@03b592c` 新建，本报告只新增文档，未改动产品代码）
> 评估日期：2026-09-13
> 范围：仓库全量（后端 `web_sa/`、前端 `frontend/modern/src/`、`tests/`、`tools/`、构建与文档）
> 性质：**评估 + 优化建议**，不含实现。每条结论都附可复现证据。

---

## 0. 结论

整体判断：这是一个**成熟度明显高于同规模个人项目**的代码库。分层意图清晰、文档齐全、
纯函数 DSP 层有单测、数据面（背压/协议/恢复）经过真实硬件打磨，工程质量门禁本地全绿
（pytest 87、ruff clean、vitest 115、`tsc --strict` 通过）。

主要问题不在"能不能跑"，而在**可演进性**：

1. **没有 CI**，所有质量门依赖人工执行 `./test.sh`；
2. **前端 UI/编排层几乎没有自动化护栏**（115 个测试全在纯 DSP/工具层），
   而 UI 层恰恰是 churn 最高、最容易回归的地方；
3. **前后端各自实现同一套二进制协议**，没有契约测试，错帧会被静默丢弃；
4. **"唯一 DLL 接触点"这一自我约束已被打破**（`rta.py` 三处直接 `import htra_api`）；
5. **两个上帝模块**：后端 `web/ws.py:_dispatch`（313 行 if/elif 链）、
   前端 `ui/controls.ts`（1547 行 / 39 条 import）；
6. **前端 14 条循环依赖**，`render/spectrum.ts` 是枢纽；
7. **i18n 字典结构危险**：452 个英文键中只有 **1 个**缺中文（首版报告写"缺 77 个"是提取脚本的
   缺陷，见 §8.1-4）；真正的问题是字典被拆成 13 个 `Object.assign` 块、`I18nKey` 类型只覆盖
   基础字面量的 194 个键、且有 3 对重复键，且没有任何一致性测试。

评分（10 分制，主观但基于上述证据）：

| 维度 | 分数 | 说明 |
|---|---|---|
| 功能完备度 | 9 | 覆盖 SWP/RTA/SDR/测量/触发/限制线，文档同步 |
| 分层与边界清晰度 | 6 | 目录分层清晰，但后端命令层/硬件层、前端编排层存在实际越界 |
| 模块内聚 / 耦合 | 5 | 上帝模块 + 14 条前端循环依赖 + 全局可变单例 |
| 数据面设计（协议/背压/恢复） | 9 | latest-wins、FREQ 保留、音频 FIFO、supervisor 恢复，设计到位 |
| 测试有效性 | 6 | 纯函数层很好；协议契约与 UI 编排层缺失 |
| 工程化（CI/依赖/版本/发布） | 4 | 无 CI；依赖声明不完整；版本号 4 处重复；无 CHANGELOG |
| 可观测性 / 可诊断性 | 8 | `faulthandler`、结构化 STATUS、health 字段、探针脚本齐全 |
| 安全默认 | 8 | loopback 校验、token、Origin 白名单、静态路径穿越防护 |
| 文档质量 | 8 | 双语、覆盖设计取舍与已知限制；扣分在无同步校验 |
| 性能 / 资源 | 7 | 主要瓶颈已处理；仍有每客户端重复序列化、画布缩放模糊 |

**最值得马上做的三件事**：加 CI（含 i18n parity 与协议契约测试）、
收敛硬件访问与命令层、给前端 UI 编排层补护栏（先协议解析单测，再 mock 后端 e2e）。

---

## 1. 评估方法与基线

评估方式：全量阅读核心模块 + 静态度量（AST/正则脚本）+ 实际执行全部质量门。
未做动态硬件测试（本机无射频信号，SDR/RTA 实时行为以文档与单测为准）。

当前基线（本次实际执行，全部通过）：

```
python3 -m pytest tests/ -q          -> 87 passed
python3 -m ruff check web_sa tests tools -> All checks passed
npm test (vitest run)                -> 12 files / 115 tests passed
npx tsc --noEmit                     -> 通过（strict: true）
```

规模（`git ls-files` 统计）：

| 区域 | 行数 | 文件数 | 备注 |
|---|---|---|---|
| `web_sa/`（后端） | 5,296 | 26 | 最大：`sdr.py` 1000、`device.py` 921、`ws.py` 604 |
| `frontend/modern/src/`（不含测试） | 9,826 | 60 | 最大：`controls.ts` 1547、`spectrum.ts` 728、`i18n.ts` 676 |
| `frontend/modern/src/__tests__/` | 1,386 | 12 | 全部为纯函数测试 |
| `tests/`（后端） | 1,299 | 11 | |
| `tools/`（探针 + e2e） | 2,700 | 20 | `sdr_probe/` 14 个脚本 + 1 个 Playwright e2e |
| `frontend/modern/index.html` | 646 | 1 | 206 个 `id`、112 个 `data-action` |
| `htra_api.py`（厂商） | 924 | 1 | 不应改动 |
| 提交历史 | 221 commits | — | 当前版本 1.5.5 |

---

## 2. 做得好的地方（应先固定住，避免重构时破坏）

| 编号 | 优点 | 证据 |
|---|---|---|
| S-1 | **数据面设计成熟**：每客户端单一发送者、POWR/RTAF latest-wins、FREQ 单独保留（丢功率不会孤立频率轴）、音频小 FIFO、STATUS 合并 | `web/client_stream.py:25-160`；测试 `tests/test_client_stream.py` 6 例 |
| S-2 | **可靠性工程实在**：supervisor 只在原生崩溃/超时码上重启并指数退避；采集看门狗按扫描时间缩放；会话快照恢复；设备级可重入锁串行化所有 DLL 调用 | `supervisor.py:18-19,49-53`、`publisher.py:18-24`、`measurements/base.py:33-98`、`device.py:169-174` |
| S-3 | **参数槽位模型**（`confirmed`/`desired`/`epoch` + TTL + 单一持久化入口 + 作用域 `resetAll`）解决了真实反复出现的 bug 类，且有 15 例单测 | `core/params.ts`、`__tests__/params.test.ts` |
| S-4 | **纯函数 DSP 层可测**：S-G 平滑、三阶段峰值、抛物线拟合、限制线、电平交叉、通道测量都是纯函数并被单测覆盖 | `dsp/*.ts`、`__tests__/{dsp,limits,levelCross,channel,grid}.test.ts` |
| S-5 | **安全默认**：非回环监听必须配 token（或显式 opt-in）、Origin 白名单、静态路径 `commonpath` 校验、`nosniff`/`DENY` 头 | `config.py:validate`、`web/http_api.py:30-58,265-272`；测试 3 例 |
| S-6 | **可诊断性**：`faulthandler` 打印原生崩溃时的 Python 栈、STATUS 暴露 `rta_health`/`sdr.health`/`status_warning`、`tools/sdr_probe/` 有系统化探针 | `main.py:76-79`、`http_api.py:60-67` |
| S-7 | **文档化设计取舍**：`ARCHITECTURE.md`/`MODE_STATE_FLOW.md`/`KNOWN_ISSUES.md`（23 条实测限制）双语 | `docs/{en,zh-CN}/` |

---

## 3. 问题清单

严重度：**P0** = 影响交付可靠性/正确性，应优先；**P1** = 结构性，持续拖慢开发；
**P2** = 卫生/优化。

### P0

#### P0-1 i18n 字典：结构缺陷 + 缺 1 个中文键（首版报告的"77 个"是错的）

- **现象**：`t()` 在 zh 缺失时回退英文（`i18n.ts:171-179`）。字典由基础字面量
  （`i18n.ts:5-164`）+ **13 个** `Object.assign(dict.en|zh, {...})` 块拼接而成。
- **证据（修正后）**：用括号配对提取（而非首版报告里那条有缺陷的正则，它漏掉了含 `-` 的键并
  在嵌套花括号处截断，因此把 1 个缺键算成了 77 个）：重构前实际是 **en 452 / zh 451**，
  唯一缺的是 `sdr_snap_tip`（"设为当前标记"）。重构后为 452/452（§9.1）。
- **真正的缺陷（与缺键数量无关）**：
  1. `type I18nKey = keyof typeof dict['en']`（`i18n.ts:166`）在 `Object.assign` **之前**
     求值 ⇒ 类型只覆盖基础字面量的 **194/452** 个键，拼错新增键 tsc 不会报；
  2. `avg`、`tip_gain`、`tip_lang`、`tip_theme`、`tip_trg_edge` 共 **5 个键**存在重复定义，
     运行时由后者静默覆盖前者；
  3. 没有任何一致性测试，双语漂移不会被发现（`ui/` 大量文案靠 `data-i18n` 运行时取值，
     缺键只表现为"界面显示英文"，不报错）。
- **影响**：文案类缺陷无法在 CI 中被发现；类型保护形同虚设。
- **建议**（已在 §9.1 实施）：
  1. 合并为单一声明式字典，删除 `Object.assign`，让 `keyof typeof dict.en` 覆盖全量键；
  2. 补 `__tests__/i18n.test.ts`：键集合相等、无空值、`{placeholder}` 两侧一致、
     类型覆盖旧 assign 块里的键；
  3. 缺的 `sdr_snap_tip` 一并补上。

  复现（修正后的脚本；键名可含 `-`，且必须在括号配对范围内取块，否则会漏键并把结果算错）：
  ```bash
  python3 - <<'PY'
  import json, re, subprocess
  src = open('frontend/modern/src/core/i18n.ts', encoding='utf-8').read()

  def block(s, i):                      # 字符串感知的括号配对
      depth = 0; j = i; q = None; esc = False
      while j < len(s):
          c = s[j]
          if q:
              if esc: esc = False
              elif c == '\\': esc = True
              elif c == q: q = None
          else:
              if c in ('"', "'"): q = c
              elif c == '{': depth += 1
              elif c == '}':
                  depth -= 1
                  if depth == 0: return j + 1
          j += 1

  segs = []
  m = re.search(r'const dict = \{', src)
  segs.append(src[m.start():block(src, src.index('{', m.start()))] + ';')
  for m in re.finditer(r'Object\.assign\(dict\.(en|zh),', src):     # 仅旧结构需要
      i = src.index('{', m.end()); segs.append(src[m.start():block(src, i)] + ');')

  js = '\n'.join(segs) + """
  const flat = {};
  for (const l of ['en', 'zh']) flat[l] = Object.keys(dict[l]);
  process.stdout.write(JSON.stringify(flat));"""
  r = subprocess.run(['node', '-e', js], capture_output=True, text=True)
  if r.returncode: raise SystemExit(r.stderr[:400])
  d = json.loads(r.stdout); en, zh = set(d['en']), set(d['zh'])
  print('en', len(en), 'zh', len(zh), 'en-only', sorted(en - zh))
  PY
  ```

  **教训**（写入 §8.1-4）：支撑结论的提取脚本必须先用它在"已知为真"的样本上自检；
  本轮评估的错误结论就来自一条未自检的正则。

#### P0-2 前端 UI/编排层没有自动化护栏

- **现象**：61 个前端源码模块中只有 **23 个（38%）**被单测直接引用；未覆盖的 38 个集中在
  `render/`（7/7 全无）、`meas/`（6/6 全无）、`ui/`（13/19，含 `controls.ts`/`keypad.ts`/`trigger*.ts`/`limits.ts`）
  以及 `core/ws.ts`、`core/i18n.ts`、`core/theme.ts`、`core/fmt.ts`、`core/markerCommon.ts`、
  `dsp/normalize.ts`、`audio/sdrAudio*`。已覆盖的是 `dsp/*`、`core/{params,level,frequency,units,refclock,store}`
  与 `ui/{displayRef,graphMode,sdrState,swpState,normPub,railMath}`（即状态机制与纯 DSP 层）。
  唯一的端到端回归 `tools/e2e/state_regression.py` 需要真机 + 运行中的服务，
  被刻意排除在 `test.sh` 之外（`test.sh:20-26`）。
- **证据**：按 `__tests__/*.test.ts` 的 import 目标统计（脚本见附录 B）。
  `tools/e2e/state_regression.py:31-36`（`from playwright.sync_api import ...`，需活设备）。
- **影响**：`controls.ts`（59 次提交）、`index.html`（67 次提交）、`core/ws.ts`（48 次）
  这些最高 churn 文件完全没有自动回归；改错只有人工点击才发现。
- **建议**（按性价比排序）：
  1. 先给 **帧解析** 补单测（见 P0-3），这是纯函数、零依赖、收益最大；
  2. 再给 `updateStatus()`/`processTraces()` 补"喂 STATUS JSON + 合成帧 → 断言槽位与
     store" 的集成测试（jsdom 已就位）；
  3. 最后引入 **假后端**（`web_sa/hardware/device.py` 已有明确 mock 边界，
     只需替换 `sdk_bindings` 的 DLL 入口；或直接起一个只发合成帧的 aiohttp stub），
     让 Playwright e2e 进 CI。

#### P0-3 二进制协议在两种语言里各写一遍，且无契约测试

- **现象**：magic/偏移/dtype 在 Python 与 TS 双份定义：
  `FREQ`/`POWR`（`measurements/framer.py:14-36` ↔ `core/ws.ts:372-385`）、
  `RTAF`（`measurements/rta.py:510-522` ↔ `core/ws.ts:162-190`）、
  `AUDF`（`measurements/sdr.py:76-78` ↔ `core/ws.ts`）。
  TS 侧对长度不符**静默 `return`**（`ws.ts:175,373,380`），错帧表现为"画面不动"。
- **证据**：`tests/test_framer.py` 只断言 dtype（3 例），没有 golden 字节向量；
  `ws.ts` 解析无单测。
- **影响**：一次字段顺序/长度改动可静默破坏显示，且难定位（无错误、无日志）。
- **建议**：
  1. 把帧规格收敛为**单一来源**（如 `docs/API.md` 中的表格 + 一个
     `protocol.json`/常量文件），Python/TS 都从它派生常量（至少自动生成头部结构体）；
  2. 两端各加 golden 测试：Python 生成字节 → 存为 fixture；TS 测试解析该 fixture
     断言数值；反向再各一次；
  3. TS 解析失败改为可观测（`console.warn` 限流 + 计数进 `dataset`），便于现场诊断。

#### P0-4 依赖声明不完整，干净环境跑不通 `./test.sh`

- **现象**：`test.sh` 调用 `python3 -m ruff`，`build.sh` 调用
  `python3 -m fontTools.subset`，e2e 需要 `playwright`；三者都**不在**
  `requirements.txt`（只列 aiohttp/numpy/pytest/pytest-asyncio/pyserial），
  ruff 仅出现在 `pyproject.toml` 的 `[project.optional-dependencies] dev`。
- **影响**：新机器/新贡献者按 README 执行 `pip install -r requirements.txt && ./test.sh`
  必失败；CI 若照抄也会失败。
- **建议**：`requirements.txt` 拆为 `requirements.txt`（运行时）+
  `requirements-dev.txt`（ruff/playwright/fonttools/pytest…），或统一指向
  `pip install -e '.[dev]'`，README 同步。

#### P0-5 没有 CI，也没有版本单一来源

- **现象**：仓库无 `.github/`、无任何 CI 配置；版本号硬编码 4 处
  （`pyproject.toml:3`、`frontend/modern/package.json:3`、
  `frontend/modern/index.html:640`、发布提交信息），无 `CHANGELOG.md`，
  发布靠人工按 git log 里 `chore(release): 1.5.5` 的模式操作。
- **影响**：版本漂移、忘记跑测试、忘记 `npm run build`（历史 TODO 里已明确记录
  "忘了 build 等于测旧包 —— 本会话犯过"）。
- **建议**：
  1. 加最小 CI（GitHub Actions 或本地 `pre-push` hook）：pytest + ruff + tsc + vitest，
     不需要硬件即可全绿；
  2. 版本单源：`pyproject.toml` 为主，构建时注入 `index.html` 的
     `tip_version`（或由 `package.json` 生成 TS 常量），发版脚本一次改完并生成
     CHANGELOG 段落。

### P1

#### P1-1 后端命令层名不副实：`ws.py` 实际承担"命令总线"，且与 HTTP 层循环依赖

- **证据**：`web/http_api.py:17` 顶层 `from .ws import CommandError, _dispatch, ...`；
  `ws.py:268,272` 在函数内 `from .http_api import build_status`（为绕开循环）。
  模块级循环依赖已由脚本确认（`http_api ⇄ ws`）。
- **影响**：命名误导（改 HTTP 传输要动 `ws.py`）；循环导入靠"函数内 import"维持，
  新人极易踩坑；`_dispatch` 同时承载校验、权限、状态写入、硬件调用、返回码，
  单测只能整体打桩。
- **建议**：抽 `web/commands.py`：
  ```
  @command('SET_RBW', validate=..., modes=('std','rta'), session_exclusive=...)
  async def set_rbw(dev, data): ...
  ```
  由 registry 统一完成 P0 校验/模式互斥/会话互斥（现在散在 `_validate_command` 与
  `_dispatch` 的 4 处 `if`），`ws.py`/`http_api.py` 退化为纯传输适配。

#### P1-2 命令分发是 313 行 if/elif 链，`_validate_command` 另有 135 行

- **证据**：`web/ws.py:292-604`（`_dispatch`）、`web/ws.py:105-240`（`_validate_command`）、
  `web/ws.py:50-64`（`_COMMANDS` 集合）。
- **影响**：新增/修改一条命令要改 **3 处**，字段校验和业务逻辑分离在不同函数里，
  参数语义容易漂移（例如 `SET_SWEEP` 的 mode 取值在 validate 与 dispatch 各写一份）。
- **建议**：与 P1-1 合并为声明式 registry，每个命令自带 schema（`_number`/`_choice`
  已是好用的原语，保留）；补一张"命令 → 校验 → 处理器"的生成式测试表，
  确保 `_COMMANDS` 与 registry 不脱节（用测试断言而非人工同步）。

#### P1-3 "唯一 DLL 接触点"约束已被打破

- **证据**：`hardware/sdk_bindings.py:1-10` 声明是本项目唯一直接接触
  `libhtraapi` 的模块；但 `measurements/rta.py:96,366,390` 三处
  `import htra_api as T` 并直接调用 `T.dll.RTA_*`、`T.pointer`、
  `T.RTA_FrameInfo_TypeDef`；`sdr.py`/`ddc.py` 用 `T = sb` 别名间接取 `sb.dll`；
  `phase_noise.py`、`device.py` 也直接调 `sb.dll.*`（走的是 re-export，边界更模糊）。
- **影响**：换库/模拟硬件需要改多个模块；pypy 化或做纯软件仿真时无法只替换一层。
- **建议**：
  1. `sdk_bindings` 补齐 `RTA_FrameInfo_TypeDef` 等缺失 re-export，
     目标：其它模块只允许 `from ..hardware import sdk_bindings as sb`；
  2. 加约束测试（或 `import-linter` 契约）：断言 `web_sa` 中除 `sdk_bindings.py`
     外无 `import htra_api`、无直接 `dll.` 访问；
  3. 顺手把 `T = sb` 这类"别名即硬件层"的写法改成显式 `sb.dll.X`。

#### P1-4 前端 14 条循环依赖，`render/spectrum.ts` 是枢纽

- **证据**（脚本检测，见附录 B）：
  `meas/harmonic.ts ⇄ render/spectrum.ts ⇄ meas/phaseNoise.ts ⇄ ui/measure.ts`、
  `render/spectrum.ts ⇄ {dsp/peaks, meas/amplitude, meas/channel, meas/harmOverlay,
  meas/harmOverlay2, render/markerTable, render/peaklist}`、
  `dsp/traces.ts ⇄ dsp/normalize.ts`、`core/ws.ts ⇄ ui/controls.ts`。
- **影响**：ESM 循环在求值顺序上不可控（`import` 期取到的可能是未初始化绑定），
  模块无法独立测试，拆分/移动代码成本高。
- **建议**：把 `spectrum.ts` 拆成
  (a) `render/orchestrator.ts`（只做"按模式选择绘制者"）与
  (b) 各测量模块**只导出纯数据计算 + 纯绘制函数**（不再回读 spectrum 的全局）；
  循环可用 `madge --circular`（加入 CI）作为验收门槛：目标 0 条。

#### P1-5 `ui/controls.ts` 上帝模块 + `index.html` 字符串耦合

- **证据**：`controls.ts` 1547 行、39 条 import、35 个模块；
  `bindActions()` 232 行（`:1141`）、`bindCanvas()` 100 行（`:1398`，且以 `if (!canvas) return` 静默返回）；
  `index.html` 646 行含 206 个 `id`、112 个 `data-action`；
  全前端 295 处 `getElementById/querySelector` 以字符串字面量取元素。
- **影响**：`index.html` 是提交次数最多的文件（67 次）；改一个 id 不会有任何编译错误，
  只会在运行期静默失效（`?.` 或 `if (el)` 风格会掩盖）。
- **建议**：
  1. 按面板域拆分 `controls.ts`（`ui/panels/freq.ts`、`refLevel.ts`、`sdr.ts`、
     `trigger.ts`、`amp.ts`…），共享 `data-action` 注册表；
  2. 建立"必需 id 清单"并在 `main.ts` 启动时自检：缺失即 `throw`（快速失败优于静默）；
     进阶做法是改用 `data-field="center"` 之类语义属性，id 只留给需要锚点的元素。

#### P1-6 全局可变单例仍是前端状态主体

- **证据**：`core/store.ts` 被 **33 个模块**导入，含 ~70 个 `export let` 与
  62 个 `export function setX`；`params.ts` 的槽位模型目前只覆盖
  `freqState`/`refState`/`swpState`/`sdrState`/`displayRef`/`graphMode`。
  触发（`swpArmed`/`trigHit`/`trigOverlay`）、瀑布（`wfLoDbm`/`wfHiDbm`/`waterfallOn`）、
  测量结果（`harm`/`pnmData`/`chanRes`/`m3dB`/`peakMarks`）、`traces`/`markers` 等
  仍是裸可变量，由多个模块直接写。
- **影响**：这正是 `params.ts` 注释里描述的"多写者、无所有者"bug 类的温床；
  迁移只做了一半，规则不统一会让后来者不知道该用哪种写法。
- **建议**：
  1. **参数**继续按 `params.ts` 迁移（RTA 中心/跨度、触发阈值组、瀑布范围）；
  2. **测量结果/迹线数据**不要塞进参数槽——它们是"数据仓库"，建议单独
     `core/results.ts`（有明确 `set/get/subscribe`，不做 desired/confirmed 语义）；
     明确写进 `ARCHITECTURE.md`，形成"参数用槽、结果用仓库"的统一规则。

#### P1-7 `build_status` 通过私有属性读取内部状态

- **证据**：`web/http_api.py:70-215`（146 行）中
  `getattr(dev, '_auto_ref', {})`、`getattr(dev, '_pending_auto_ref', None)`、
  `getattr(session, '_error_streak')`、`_recovery_attempts`、`_packets_ok/_err`
  （`:60-67`）。
- **影响**：序列化层与内部实现耦合，重命名私有字段不会有编译期/测试期报警
  （`getattr` 默认值把错误吞掉，只会显示 0/空）。
- **建议**：把对外可见的诊断内容显式化：`DeviceState.auto_ref_view()`、
  `Session.health()`、`DeviceState.status_warning`（已有）；`build_status` 只做字典组装。

#### P1-8 `DeviceState` 上帝对象 + `HarogicDevice` 上帝类

- **证据**：`DeviceState`（`device.py:51-153`）约 100 个字段，横跨 SWP/RTA/SDR/触发/
  GNSS/校准/测量结果；`HarogicDevice`（`device.py:159-921`）同时负责生命周期、
  SWP profile 构造、缓冲区管理、自动参考控制环、GNSS 查询、参考时钟校准、会话宿主。
  其中自动参考是一个隐性状态机（`_auto_ref` 字典 + `_auto_ref_geometry_seen` +
  `_pending_auto_ref`，`_observe_reference_peak_locked` 76 行）。
- **影响**：任何一处改动都要读 900 行上下文；单测只能覆盖到局部
  （现有 `test_device_state.py` 13 例已尽力）。
- **建议**（分步，风险可控）：
  1. 先抽 `AutoReferenceController`（输入：迹线+几何+模式；输出：待应用的 ref），
     它天然可单测，是把最难的状态机从 IO 类里剥出来的最大收益；
  2. 再按模式把 `DeviceState` 拆为 `SwpParams`/`RtaParams`/`SdrParams`/`TriggerParams`
     子结构（`STATUS` 形状保持不变，前端无感）；
  3. `HarogicDevice` 只保留"生命周期 + 会话宿主 + 一次 step"。

#### P1-9 会话生命周期隐式，且靠私有标志握手

- **证据**：`measurements/__init__.py:16-24` 的 `make_session()` 内部直接
  `dev.session.exit()`；`ws.py:528-539` 在切换前直接置
  `old_sess._ready = False`，并在之后读 `sess._ready` 判断是否成功。
- **影响**：模式切换的"进入/退出/就绪"三态没有显式接口，失败路径靠约定
  （`_ready` 是私有属性，SDR/RTA 各自维护）。
- **建议**：定义 `SessionManager.switch(name) -> Session`，会话暴露
  `enter()/exit()/is_ready()`；`SET_MODE` 只调用 manager，切换失败抛
  `CommandError`（现在已有 `mode_not_ready`，把它变成接口契约而非属性约定）。

#### P1-10 进程级自杀散落在业务代码里

- **证据**：`os._exit(70)` 出现在 `web/publisher.py:69,89`、`web/ws.py:330,388`；
  `supervisor.should_restart()` 依赖退出码 70/负数。
- **影响**：退出码是跨进程契约，却硬编码在 4 个业务位置；单测很难覆盖"该退出"的路径；
  日志格式不统一。
- **建议**：集中为 `web/recovery.py: fatal(reason) -> NoReturn`（记录统一格式、
  带上 last_error、`os._exit(70)`），业务代码只调用它；`should_restart` 的输入
  也改为常量 `EXIT_FATAL = 70`。

#### P1-11 重复的 JSON 规范化 + 每客户端重复序列化

- **证据**：`client_stream._finite_json`（`:14-22`）与 `http_api._json_safe`（`:20-28`）
  语义相同；`ClientStream.publish_json`（`:87-101`）对**每个客户端**各做一次
  `json.dumps`（N 个客户端 = N 次序列化 + N 次递归有限性检查）。
- **影响**：当前客户端数很少，影响可忽略；但这是"扩展时才会痛"的隐性成本，
  且两份实现可能漂移（一个改 NaN 策略另一个没改）。
- **建议**：抽 `web/jsonutil.py`；publisher 对外 `broadcast_json(obj)`：
  序列化一次 → 复用字符串写入各客户端队列。

### P2

| 编号 | 问题 | 证据 | 建议 |
|---|---|---|---|
| P2-1 | `store.ts` 在**导入期**访问 DOM（`document.getElementById('spectrum')`），使模块导入带副作用，测试必须 jsdom | `core/store.ts:34-36` | 改为 `initStore()`/惰性 getter；`canvas/ctx/W/H` 由 `init` 注入 |
| P2-2 | 58 个 `controls.ts` 导出符号无任何外部引用（API 面污染）；`noUnusedLocals/Parameters=false` 掩盖未用局部 | 脚本检测（附录 B） | 去掉不必要的 `export`；开启 `noUnusedLocals` 并逐步清理 |
| P2-3 | 前端无 lint/format（无 ESLint/Prettier 配置），风格靠约定 | `package.json` scripts 仅 dev/build/preview/test | 引入 ESLint（typescript-eslint）+ Prettier，纳入 CI；风格类规则先 warning |
| P2-4 | 画布固定 860×480 但 CSS `width:100%` 缩放，无 `devicePixelRatio` 后端缓冲 → 宽屏/高分屏模糊 | `index.html:76`、`style.css:219-221` | 按容器尺寸 + DPR 设置 `canvas.width/height`（渲染坐标保持逻辑像素）；`store.W/H` 随之改为动态 |
| P2-5 | 双语文档无同步校验，已出现长度差异（`ARCHITECTURE` en 126 / zh 118 行；`KNOWN_ISSUES` 62/51） | `wc -l docs/{en,zh-CN}/*.md` | 在 CI 加一个轻量检查：两侧**小节标题**集合一致（内容可意译，结构必须一致） |
| P2-6 | 27KB 临时备忘 `TODO-frontend-state.md` 留在仓库根，靠 `.git/info/exclude` 排除（本地私有） | 文件头注释 | 已结项内容归档到 `docs/dev/`（或删除）；未结项项转成 issue/`docs/dev/ROADMAP.md`，避免"文档说未做、代码已做"的漂移（本报告第 4 节就是实例） |
| P2-7 | 后端测量模块覆盖不均：`harmonic.py`、`phase_noise.py`、`main.py`、`logging_setup.py` 无直接单测 | 测试名清单 | 补 harmonic/PNM 的"结果解析/参数裁剪"单测（无需硬件，喂合成结构体） |
| P2-8 | 遗留 helper 与探针脚本混放 | `config.py:153-154` `fit_span` 自注 "legacy helper"；`tools/sdr_probe/` 14 个脚本 | 删除确认无引用的 helper；探针脚本移到 `tools/sdr_probe/`（已在此）并在 `tools/README` 说明用途与是否需要硬件 |
| P2-9 | 每条命令都全量构建 STATUS（146 行 dict + 递归有限化），高频命令浪费 | `ws.py:274`、`http_api.py:70` | 只回显与命令相关的字段（或复用上次序列化结果）——注意：前端依赖完整 STATUS 做槽位确认，改动需先看协议 |

---

## 4. 重构路线图

原则：**先建护栏，再动结构**；每阶段结束时 `./test.sh` 与 `tsc` 必须全绿，
且每阶段都应当是"可独立合并"的。

### Phase 0 — 基线固化（0.5–1 天，零风险）

| 任务 | 产出 | 验收 |
|---|---|---|
| 加 CI（pytest + ruff + tsc + vitest） | `.github/workflows/ci.yml` 或 `pre-push` hook + README 说明 | 干净 venv 中 CI 全绿 |
| 依赖声明补全 | `requirements-dev.txt`（ruff/playwright/fonttools/pytest*） | `pip install -r requirements.txt -r requirements-dev.txt && ./test.sh` 通过 |
| i18n parity 测试 | `__tests__/i18n.test.ts` | 先暴露结构问题（类型只覆盖 194/452、5 个重复键），合并字典后通过 |
| 协议 golden 测试骨架 | Python 生成 fixture + TS 解析断言（FREQ/POWR/RTAF/AUDF） | 4 类帧双端一致 |
| 循环依赖门槛 | `madge --circular` 输出当前 14 条作为**基线**（只允许减少） | CI 中记录基线数 |
| 版本单源 | 构建注入版本 + CHANGELOG 模板 | `index.html` 不再硬编码版本 |

### Phase 1 — 低风险一致性（2–4 天）

i18n 合并为单一字典并类型化；删除 P2-2 的死导出；`store` DOM 懒初始化（P2-1）；
`fatal()` 集中（P1-10）；`_finite_json` 去重 + 单次序列化广播（P1-11）；
`build_status` 改为显式 view 接口（P1-7）。

验收：无行为变化的纯重构；`test.sh`/`tsc`/e2e 全绿。

### Phase 2 — 后端命令层与硬件边界（1–2 周）

1. 新增 `web/commands.py` registry（名称/校验/模式约束/会话互斥/处理器），
   `ws.py`、`http_api.py` 只做传输（P1-1、P1-2）；
2. `sdk_bindings` 补齐 RTA 符号，禁止其它模块 `import htra_api`，
   加约束测试（P1-3）；
3. `AutoReferenceController` 抽取 + 单测（P1-8 第一步）；
4. `SessionManager` 显式化（P1-9）。

验收：现有 87 个后端测试**不改断言**即通过（仅允许 import 路径调整）；
新增命令只需加一个 registry 条目 + 一条表驱动测试；约束测试证明无越界 DLL 访问。

### Phase 3 — 前端解耦（2–3 周）

1. 打破 `spectrum.ts` 枢纽（P1-4），`madge --circular` 归零；
2. 按面板拆分 `controls.ts`，`index.html` 必需 id 自检（P1-5）；
3. 剩余参数迁移到 `params.ts`，测量结果迁到 `core/results.ts`（P1-6）；
4. 帧解析 + `updateStatus` 集成单测（P0-2 步骤 1-2）；
5. 画布 DPR/resize（P2-4）。

验收：`madge --circular` = 0；vitest 覆盖新增的解析/状态测试；
e2e（真机）仍全绿。

### Phase 4 — 按需

假后端 + Playwright 进 CI（P0-2 步骤 3）；`DeviceState` 分模式拆分（P1-8 第二步）；
文档结构一致性检查（P2-5）。

> **与第 7 节的关系**：从“方便后续扩展”的角度，还缺五个扩展点（E-1 参数 schema、E-2 能力表、
> E-3 会话 Protocol 去模式分支、E-4 帧 codec 表、E-5 前端注册点）。它们与 Phase 2/3 重叠，
> 建议**合并执行**而不是另开一轮：E-1/E-2 随命令 registry 一起做，E-3 随 publisher 去分支一起做，
> E-4/E-5 归入 Phase 3。

---

## 5. 快速收益清单（半天内可完成，风险极低）

1. `requirements-dev.txt` 补 ruff/playwright/fonttools（P0-4）——否则 README 的测试步骤是错的。
2. `__tests__/i18n.test.ts` + 合并字典并补上缺失的 1 个中文键（P0-1）。
3. `web_sa/hardware/sdk_bindings.py` 补 `RTA_FrameInfo_TypeDef` 等 re-export，
   把 `rta.py` 三处 `import htra_api as T` 改为 `_sb`（P1-3，纯机械）。
4. 删除 `controls.ts` 中无外部引用的 `export`（P2-2）。
5. `supervisor.EXIT_FATAL = 70` + `fatal()` 集中（P1-10）。
6. 把 `TODO-frontend-state.md` 中已结项部分归档（P2-6）——本报告已发现其
   "仍未做：SWP/RTA 迁移"与代码现状（`swpState`/`freqState`/`displayRef`/`graphMode`
   均已槽位化）矛盾。

---

## 6. 非目标（建议明确不做）

1. **不引入前端框架/状态库**（React/Vue/Redux 等）。原生 DOM + `params.ts` 已能表达
   本项目所需的单所有者状态模型；重写成本远大于收益，且会丢掉现有 e2e 契约。
2. **不为了覆盖率写 UI 快照测试**。优先协议契约测试与状态机单测；
   UI 层用 Playwright 断言"可观测行为"（既有 e2e 的方向是对的，应该扩大而不是替换）。
3. **不修改 `htra_api.py`**（厂商文件，HAROGIC 版权）。所有适配放 `sdk_bindings`。
4. **不在没有明确需求前拆分进程/做多进程服务化**（`KNOWN_ISSUES.md` 第 1 条已把
   "Web/SDK 进程分离"推迟到 VSA 阶段）。`os._exit` + supervisor 的现状是**有意的**
   工程取舍，不应视为缺陷。
5. **不为"整齐"统一命名/目录**。循环依赖、上帝模块、越界访问这些**有具体代价**的
   问题优先；纯风格问题交给 lint。

---

## 7. 模块化与功能扩展性补充

前面各节按“当前代码有什么问题”展开；本节换一个尺度：**新增一个功能的改动面有多大**。
这才是模块化真正的验收指标——目录分层只是第一步，**扩展点（seam）**才是关键。

### 7.1 用“改动面”度量模块化（历史实测）

| 新增的东西 | 实际改动 | 文件数 | 说明 |
|---|---|---|---|
| 一个新参数（SWP 检波器 `SET_DETECTOR`） | `web/ws.py`（命令集 + 校验 + 分发）、`hardware/device.py`（状态+profile 应用）、`web/http_api.py`（STATUS 字段）、`frontend/index.html`（控件）、`core/i18n.ts`（2 处）`core/ws.ts`、`ui/controls.ts` | **8**（+1 测试） | 提交 `12b5b72`：52 行改动铺在 8 个文件 |
| 一个新硬件模式（SDR） | `demod/`（5 文件）、`sdk_bindings`、`device.py`、`measurements/{sdr,__init__}`、`web/{ws,http_api,publisher,client_stream}`、前端 `{ws,controls,audio}` = **16 个产品文件**，另加 15 个探针/文档文件 | **31**（共 1837 行） | 提交 `ce92d7a` |
| 一个新帧类型 | 编码器 + `web/client_stream.py` 的保留策略分支 + `core/ws.ts` 解析分支 + 测试 | **4** | 无 codec 注册表 |
| 一个新设备型号（如 SAN-200） | `config.py`（表）、`ws.py`（大量硬编码限值）、`rta.py`（`FULL_SPAN_HZ`/`DISPLAY_POINTS`）、`http_api.py`（`rta_defaults`/`points`）、前端 fallback | **5+** | 能力表不是限值唯一来源 |

结论：改动面随既有功能数量**线性增长**，因为每个扩展轴都需要修改中心 `if/elif`、中心 dict、前端分发、
两本 i18n 字典和 `index.html`。下面五个扩展点按收益排序。

### 7.2 缺失的五个扩展点

**E-1（最高收益）参数/命令没有单一 schema。**
同一个参数存在 **4 份独立描述**：① `DeviceState` 字段与默认值；② `_validate_command` 里的范围字面量；
③ `build_status` 的键名与 `req/swp/rta/sdr` 嵌套（`points: 3328`、`rta_defaults` 都是硬编码）；
④ 前端 `params.ts` 槽位 + `index.html` 控件 + i18n。`SET_DETECTOR` 改 8 个文件就是这份清单的直接后果。

建议：声明一次 `ParamSpec(name, type, min, max, unit, default, modes, scope, group, render)`，
由它派生：(a) 命令校验；(b) STATUS 结构与 `/api/schema`；(c) 前端据此**自动生成**数值/枚举/开关控件与槽位。
收益：普通参数的改动面从 8 文件降到 1–2；前端不再需要为每个参数手写控件与 i18n。
边界：只让 schema 覆盖数值/枚举/开关；图形、上下文相关按钮仍需手写，不要过度生成。

**E-2 设备能力不是限值的唯一来源。**
`ws.py` 的校验里硬编码了 `rbw ≤ 10e6`、`points ≤ 4000`、`rta span ≤ 50.78125e6`、`ifbw ≤ 500000`、
`decimate ≤ 2048`、`atten ≤ 33`、`pnm 1..9e6` 等；`50.78125e6` 在 4 个文件各写一份，
`3328` 在 `rta.py` 与 `http_api.py` 各写一份。

建议：能力集收敛到 `DeviceCapabilities`（`rbw_max`/`points_max`/`rta_span_max`/`ifbw_max`/`decimate_max`/
`demod_modes`/`features{pnm,rta,sdr,trigger}`…），未知型号给保守默认；用 `supports('pnm')` 取代 `pnm_supported` 特例。
收益：支持新固件/新型号 = 改一张表；前端也能据此禁用控件（现在是硬编码 fallback + 灰显）。

**E-3 会话接口不一致，模式策略外泄到调度层。**
`std` 没有 `reconfigure()`（走 `dev.configure_swp()`），`harmonic`/`pnm` 也没有（只有 `rta`/`sdr` 有，见 `ws.py:338-343`）；
而 `publisher.py` 里 `mode in ('rta','sdr')` 决定超时（`:19`）、`mode == 'std'` 决定 FREQ 去重（`:71`）、
`mode == 'sdr'` 决定 0/2 ms 节流（`:99`）；`ws.py` 有 8 处 `sess.name == 'rta'|'sdr'` 分支。

建议：把模式策略变成会话的属性，而不是调度层的分支：
`acquisition_timeout()`、`pacing() -> float`、`dedupe_policy()`、`reconfigure()`、`reset_defaults()`、
`is_ready()`、`status_view()`、`health()`、`param_specs()`（配合 E-1）。
收益：新增模式不再改 `publisher`/`ws`/`build_status`/`client_stream`；顺带修掉 `_ready` 私有握手（正文 P1-9）。
用 `typing.Protocol` 声明接口，测试用例据静态结构断言每个会话都满足。

**E-4 帧类型没有 codec/保留策略表。**
`client_stream.publish_bytes` 用 `if magic == b'FREQ' / elif AUDF / else latest-wins` 写死了保留语义（`:63-79`）；
新增帧型必须同时改 `client_stream`、`ws.ts`、编码器与两边测试。
建议：`FRAME_POLICY = {FREQ: retain, AUDF: fifo(20), POWR: latest, RTAF: latest}`，
未知 magic 默认 latest；TS 侧同样用一张 decode 表 + 失败计数（配合正文 P0-3 的 golden 测试）。

**E-5 前端没有注册点。**
`renderAll()` 用 `viewMode` 的 `if/elif` 手动调用各模块并手工开关 4 张表的 DOM（`spectrum.ts:383-432`）；
新增测量面板要同步改 `renderAll`、`ui/measure.ts` 的 tab、两本 i18n 字典与 `index.html`。
建议：`registerRenderer(viewMode, {render, tables, statusBlocks})` 与
`registerMeasurementTab(...)`；i18n 按 `namespace.*` 拆文件并在启动时合并，parity 测试按 namespace 检查。
收益：新增测量模块 = 新增一个文件 + 注册一行，`renderAll` 不再增长。

### 7.3 落地顺序与相互关系

1. **命令 registry（正文 P1-1/P1-2）是 E-1 的前置**：先把“校验+分发+模式约束”集中，再谈 schema。
2. **E-2（能力表）可与命令 registry 并行**，且必须先做——否则 registry 会把硬编码限值搬进新家。
3. **E-3（会话 Protocol）与 publisher 去 mode 分支同步做**，一次改完避免两轮回归。
4. **E-4/E-5 放在 Phase 3**（前端解耦）一起，共享“注册表 + 基线计数”的护栏。

**明确的“假扩展点”（不建议做）**：不做通用插件系统/动态加载（`importlib` 扫描 `plugins/` 之类）。
本项目的扩展者就是作者本人，收益低而调试成本高；也不要试图由 schema 自动生成**全部**界面。

### 7.4 扩展性验收清单（新增功能的完成定义）

- [ ] 新增参数**不改** `_dispatch`，只加 registry/spec 条目
- [ ] 新增参数的上下限来自 spec/能力表（测试断言 `ws.py` 未新增限值字面量）
- [ ] 新增模式**不改** `publisher.py`/`ws.py` 的 `mode == ...` 分支
- [ ] 新增帧型只加一张 codec 表条目 + 一处解析
- [ ] 新增面板只加一个注册项 + 一个 i18n namespace 文件
- [ ] `madge --circular` = 0，i18n parity 通过，`tsc` 未新增 `any`
- [ ] 新面板的必需 id 已进入启动自检清单（正文 P1-5）

### 7.5 可量化的护栏（建议进 CI，防回流）

| 护栏 | 形式 | 基线（当前） |
|---|---|---|
| 模式分支不增加 | 断言 `ws.py` 中 `mode ==`/`sess.name ==` 出现次数不超基线 | 13 处（`ws.py` 9 + `publisher.py` 3 + `device.py` 1） |
| 命令层不再綗胀 | 断言 `_dispatch`（313 行）与 `_validate_command`（135 行）长度上限 | 350 / 150 |
| 限值不进校验层 | 断言 `ws.py` 中 `maximum=<字面量>` 数与能力表一致 | 见 E-2 清单 |
| 循环依赖不增加 | `madge --circular` 输出数 | 14 |
| i18n 不漂移 | en/zh 键集合相等 + 占位符一致 | 评估时为 452/451（缺 `sdr_snap_tip`）；已在 §9.1 修复并在 vitest 中固化 |

---

## 8. 本评估的自我复核（对照业界最佳实践）

**结论：评估结论成立，建议方向与业界通行做法一致，未发现“反最佳实践”的建议。**
复核中修正了 1 处事实性偏差与 2 处需要限定的表述，并补齐了原评估遗漏的 3 个点。

### 8.1 已修正

| # | 问题 | 修正 |
|---|---|---|
| 1 | P0-2 称 “`render/`、`ui/`（除 rail）、`meas/` 零单测” 不准确 | 改为按模块统计：61 个模块中 23 个（38%）被单测直接引用；`ui/{displayRef,graphMode,sdrState,swpState,normPub,railMath}`、`core/{params,level,frequency,units,refclock,store}`、`dsp/*` 均有覆盖，未覆盖集中在 `render/`、`meas/`、UI 胶水层与 `core/ws.ts` |
| 2 | P0/P1/P2 未说明分级依据 | 已明确：P0/P1/P2 是**行动优先级**（P0 = 影响交付可靠性/正确性，先做），不等同于线上故障等级 |
| 3 | E-1 “由 schema 生成前端控件” 边界不清，易被误读为“全部 UI 自动化” | 已限定：schema 只覆盖数值/枚举/开关，图形与上下文相关按钮仍手写 |
| 4 | **P0-1 “缺 77 个 i18n 键”是错的**（最严重的一处）：支撑它的正则漏掉了含 `-` 的键并在嵌套花括号处截断 | 用括号配对重算：实际 en 452 / zh 451，只缺 `sdr_snap_tip`。结论从“大量缺翻译”改为“字典结构缺陷”（类型只覆盖 194/452、5 个重复键、无一致性测试）。§3 P0-1、§5、§7.5、附录 A 与本文正文已全部更正 |

### 8.2 与业界实践对照

| 建议 | 对应实践 | 判定 |
|---|---|---|
| CI（pytest/ruff/tsc/vitest） | CI gate，主干开发的最低要求 | 符合 |
| 协议 golden 测试 | 契约测试 / consumer-driven contract | 符合 |
| i18n parity + 类型化键 | i18n lint / 本地化 CI 校验 | 符合 |
| `madge --circular` = 0 | 架构守护测试（ArchUnit / dependency-cruiser 同思路） | 符合 |
| 版本单源 + CHANGELOG | SemVer + 发布自动化 | 符合 |
| 依赖声明补全 | 可复现构建 | **不完整** → 本次补 G-2 |
| ParamSpec 单一 schema | 仪器软件惯用的“命令树 + 参数元数据”（SCPI 风格） | 符合（需限定，见 8.1-3） |
| 会话 Protocol + 策略下沉到会话 | 端口-适配器 / 策略模式（Hexagonal） | 符合 |
| 不做插件系统、不引入前端框架 | YAGNI / 技术选型稳定优先 | 符合 |
| 上帝对象拆分 | SRP | 符合，但需分批（原报告已写） |

顺序上也符合通行做法：**先建护栏再重构（refactor under test）**——所以 Phase 0 必须先于 Phase 2/3。

### 8.3 原评估遗漏、本次补齐

**G-1 没有性能/资源基线。** 原报告只做静态分析，却写下“主要瓶颈已处理”这类结论，没有测量支撑。
→ 补：`tools/bench.py`（可重复的延迟/帧率/CPU 基线）+ 基线表；让“优化”类建议可验证。

**G-2 依赖未锁定。** `aiohttp>=3.9`、`numpy>=1.24` 无上界，前端 `package.json` 用 `^`（但有
`package-lock.json`），Python 侧没有约束文件。可复现构建要求锁定或至少记录“已验证版本矩阵”。

**G-3 没有硬件在环（HIL）测试入口。** 仓库有 `tools/hardware_smoke.py` 与 Playwright e2e，
但没有统一命令、没有基线、也没有“发版前必须跑”的约定。仪器类软件的通行做法是 `make hw-test`
（真机冒烟 + 状态机回归）并列入发版清单。→ 本次实现（见 §9）。

### 8.4 确认“不需要”的东西

- 不做结构化日志/分布式追踪（单用户单进程仪器，收益低）；
- 不追求 100% 覆盖率（测试投入应放在协议契约与状态机）；
- 不做依赖注入容器/框架化改造（当前构造函数注入 `dev` 已足够）。

---

## 9. 实施记录（分支 `refactor/arch-review-improvements`）

路线图 Phase 0–3 已全部实施（自评估提交起 21 个；连同 3 个评估文档提交，相对 master 共 24 个），每一步都跑 `make ci` + `make hw-test` + `make bench`。

### 9.1 已完成

| 报告编号 | 内容 | 验证 |
|---|---|---|
| P0-1 | i18n 合并为单一声明（452/452）、补上唯一缺失的中文键、`I18nKey` 覆盖全量键、parity 测试 | `__tests__/i18n.test.ts` |
| P0-3 | `core/frames.ts` 为 TS 侧唯一帧定义；golden fixture 由 Python 编码器生成、双侧断言 | 后端 6 项 + 前端 7 项 |
| P0-4 / G-2 | 依赖拆成 runtime/dev/lock 三份并加双边界 | 干净环境 `./test.sh` |
| P0-5 | 版本单一来源 + `--check` | 改坏即失败 |
| G-3 | 缺厂商库时跳过 7 个硬件模块；`make ci` / `hw-test` / `bench` | 离线 **78** 项通过 |
| G-1 | `tools/bench.py`（固定配置后测量，修掉一次 2.5× 假回归）+ 基线 | 连续多次通过 |
| P1-1/P1-2 | **命令层声明式表**（`CommandSpec` + 守卫标志，ws.py 仅传输） | 14 项表测试 + `command_sweep.py` 真机 30/30 |
| P1-3 | `rta.py` 不再直连 `htra_api`；`sdk_bindings` 补齐符号 | 守卫 3 → 0 |
| P1-4 | **前端循环依赖 14 → 0**（redraw seam + plot 几何 + 4 叶子模块） | 守卫 + 真机 UI 回归 |
| P1-5 | **`controls.ts` 拆成 9 个 `ui/panels/*`**（1548 → 934 行）+ **DOM id 契约检查**（抓到 `#cur-ifgain` 从未显示） | tsc + 真机 + `check_dom_ids.py` |
| P1-6 | **参数槽位化补完**：触发组、瀑布组（显示范围/暂停/淡出/分档）、显示组（单位/偏移/平滑）；**测量结果移入 `core/results.ts`**，共享类型移入 `core/model.ts`（store 仅 re-export，调用点不变）。规则已写入 `ARCHITECTURE.md`（参数用槽位、结果用仓库、热路径勿在循环内 `get()`）。仍留在 store 的 `spanStepHz/spanStepAuto`、`dbPerDiv`、`levelUnit` 是单一写入者的显示/前端状态，不属于本项针对的多写者 bug 类 | 141 前端测试 + 真机；见 9.3 的性能教训 |
| P1-7 | 会话 `health()`、设备 `auto_reference_view()` | STATUS 字段不变 |
| P1-8 | **`AutoReferenceController` 独立** + 16 项纯函数测试 | 新增 16 项 + 原 7 项行为测试 |
| P1-9 | **会话生命周期协议** `request_stop()`/`is_ready()` + `SessionManager` | 表测试 + 真机 |
| P1-10 | `web/recovery.py` 统一 `fatal()` | 2 项 |
| P1-11 | `web/jsonutil.py` + publisher 单次序列化广播 | 后端 + 前端测试 |
| E-1 | **`ParamSpec` schema**（边界可为 caps 回调）+ `GET /api/schema`（含鉴权） | 6 项 |
| E-2 | 硬件限值进 `DeviceCapabilities` | `test_model_limits_come_from_capabilities` |
| E-3 | 采集策略下沉会话（publisher 无 mode 分支） | `test_publisher.py` |
| E-4 | `FRAME_POLICY` 保留策略表 + 覆盖性测试 | 2 项 |
| E-5 | **三个注册点**：视图（`render/registry.ts`）、测量页签（`ui/measureRegistry.ts`）、**i18n 按域拆命名空间**（core 371 / trigger 50 / sdr 5 / limits 19 / keypad 7，启动合并） | 6 项注册表测试 + i18n parity |
| P2-1 | `core/store` 导入期不再访问 DOM（`initStore()` + 快速失败） | `__tests__/store.test.ts` |
| P2-2/P2-3 | `noUnusedLocals/Parameters`；ESLint 见 9.3（上游受阻） | tsc 干净 |
| P2-4 | **HiDPI**（backing store ×DPR，逻辑坐标不变） | store 测试 + 真机 |
| P2-5 | 双语结构一致性检查（8 对文件） | `make ci` |
| P2-7 | **测量结果组装测试**：谐波序列用 stub 设备驱动（阶次列表/dBc 参考/幅度跟随/频率上限停止/参数裁剪），PNM 载荷抽成纯函数 `pnm_payload()` | 6 项（硬件无关） |
| P2-8 | 更正 `fit_span` 并非遗留 helper | — |
| §7.5 | `tools/quality/architecture_guard.py` + baseline | `make ci` |
| **A1** | 假后端 + UI 冒烟进 CI（见 §9.3 说明）：`hardware/fake_device.py` 不 import 厂商库，`measurements/fake.py` 合成 RTAF/AUDF，`tools/e2e/ui_smoke.py` 20 项用户可见检查，CI `ui-smoke` job | 156→按需后端测试 + `make e2e-fake` 全绿 |
| **A1 延伸** | 完整 `state_regression.py`（45 项参数状态机契约）也跑在假后端上；`make e2e-fake` 串跑两个脚本，CI 同款 | 实测 45/45 通过，脚本零修改 |
| **B1** | `DeviceState` 按所有者拆分：`SwpParams`(17)/`RtaParams`(11)/`SdrParams`(14)/`TriggerParams`(12) + 扁平别名过渡；`device.py` 921→566 行 | `tests/test_device_state_grouping.py`（4 项）+ 真机 |
| **B4** | 剩余单写者客户端偏好槽位化（8 项）+ 单位表改 `unitMap`；`dbPerDiv/levelUnit/currentGapFill` 按热路径规则有意保留 | `params.test.ts`（含 TTL 不回退）+ 真机 |
| **C2** | 根目录 27 KB 临时 TODO 归档进受版本控制的文档后删除（结项结论→KNOWN_ISSUES 24–27，失败模式→DEVELOPMENT §3，诊断键→§11） | `check_docs_parity`（8 对）+ 文件已删 |
| **C4** | bench 记录周期性 STATUS 的 `stream.clients`，多客户端时警告（此前只记录设备告警） | `make bench` |
| **Auto Scale 一次性化** | Ref 的 "Auto" 改为一次性动作（`AUTO_SCALE`），带忙碌/结果提示；IF 溢出安全量程始终设置并启用（手动衰减不再使其失效），离开窗口的迹线会自动拟合；峰值表门限改为每个几何只从稳健估计（多帧中位数）拟合一次。真机实测：点击→落定 0.11-0.12 s（原 1.86 s），第二次点击报 `ok` 且不重配。SDR 设置（调谐、捕获带宽、解调、IF 带宽、去加重、音量、静噪、AGC）改为持久化偏好并在进入时重新应用；
扫频中心交接移到显式的 Shift+点击手势（此前每次进入都会重推全部设置）。随后把安全量程改为只做保护方向（IF 溢出与严重削顶；抬升 Ref 永不被撤销），并把 Auto 消息移到画布状态栈。SDR 也并到同一条 `AUTO_SCALE` 路径（此前在 TypeScript 里重复实现规则、STATUS 看不到）：后端按泛解析度迹线拟合，客户端把 target 应用到自己的显示刻度 | `make ci`、`make e2e-fake`、`make hw-test` |

### 9.2 客观进展（守卫指标，重构前 → 现在）

| 指标 | 前 | 后 |
|---|---|---|
| `frontend_cycles` | 14 | **0** |
| `backend_cycles` | 1 | 1（`http_api ⇄ ws`，传输层） |
| `dispatch_lines`（最长处理器） | 313 | **33** |
| `validate_lines`（最长校验） | 144 | **16** |
| `command_specs` | — | 24 |
| `mode_branches` | 18 | **0** |
| `htra_imports_outside_bindings` | 3 | **0** |
| `validation_limit_literals` | 35 | **0** |
| 后端测试 | 87 | **156** |
| 前端测试 | 115 | **152** |
| 硬件无关（CI 可跑）测试 | 0 | **78** |
| `ui/controls.ts` | 1548 行 | **934 行**（+ 9 面板模块） |
| `core/store.ts` | ~285 行 / 70 个可变全局 | **226 行 / 37 个**（+ `results.ts` 89 + `model.ts` 48） |
| `DeviceState` 字段 | ~100 | **83**（控制环已抽出） |
| e2e 断言数（真机） | 27 | **45** |
| 生产 bundle | 178 kB | 156 kB |

### 9.3 未做 / 受阻 / 已决策（v1.5.6 收尾复核）

已完成项见 §9.1；这里只列仍然开放、受上游阻塞或已明确决定不做的项。

| 项 | 状态 | 说明与下一步 |
|---|---|---|
| **e2e 覆盖 Firefox（A2）** | 未做（本轮未选） | 人工测试用的是 Firefox，CI 只跑 Chromium。下一步：`playwright install firefox`，把 `make e2e-fake` 的两个脚本跑成双浏览器矩阵。 |
| **P2-3：ESLint** | 受阻（上游） | `typescript-eslint@8` 要求 `typescript <6.1`，本项目用 TypeScript 7.0.2（npm ERESOLVE 已复现）。当前由 `tsc --strict` + `noUnusedLocals/Parameters` + 架构/注册/DOM-id/文档四类守卫覆盖；等上游支持后接入。 |
| **P2-9：高频命令只回显变化字段** | 未做（低收益） | 前端槽位确认依赖完整 STATUS；属性能优化而非结构问题。若将来 STATUS 变大或客户端增多再做增量字段。 |
| **前端 DOM 层单测（P0-2 剩余）** | 有意为之（策略） | 88 个模块中 31 个被单测直接引用；未覆盖集中在 `ui/`、`render/`、`meas/`、`ui/panels/`——由无硬件的 e2e 覆盖（`ui_smoke` 20 项 + `state_regression` 45 项）。只在某个模块出现纯逻辑分支时补单测，不追求模块覆盖率。 |
| **三条低优先候选**（来自已删除的临时 TODO） | 未做（记录备查） | ① `sdrPeakEma/sdrNoiseEma` 首帧种子：重配后的暂态可能用坏值初始化，中位数初始化更稳；② `web-sa-mode` 恢复时与后端实际模式的冲突处理；③ STATUS 回包与 HTTP `/api/config` 并存时的交错。 |
| **`dbPerDiv` / `levelUnit` / `currentGapFill` 保持普通值** | 已决策：有意保留 | 单写者，但位于每帧热路径（`getY`／电平换算／gap fill），在那里调槽位 `get()` 正是曾拖满主线程的做法（DEVELOPMENT §3 热路径规则）。若将来这些读取移到冷路径，再迁移。 |
| **`controls.ts` 的 11 个无外部引用导出** | 已决策：保留为公开 API | 函数本身都在用（动作表/DOM 监听/快捷键），只是没有其它模块 import；用户选择保留为公开能力导出（脚本/e2e/调试可用）。最小化 API 表面不是目标本身。 |

### 9.4 验证记录（本机，SAN-90 + tinySA 已连）

```
make ci        -> pytest 147 passed / ruff clean / i18n+frames parity / DOM id 契约 /
                  双语文档结构一致 / 架构守卫（cycles 0、mode branches 0）/ 构建成功
make hw-test   -> tinySA CW 100.2 MHz 实测 -25.7 dBm(SWP) / -25.3 dBm(RTA)
                  tools/command_sweep.py：24 条命令 + 6 条守卫拒绝全部符合预期
                  UI 状态机 39 项检查全过，无页面错误
make bench     -> 对基线通过（固定配置 points 1001/auto RBW/ref -30/atten auto/spur bypass）：
                  SWP 174 fps、RTA 214 fps、SDR 19 fps + 音频 50 fps、切换 465/310 ms
HTRA_API_LIB=/nonexistent python3 -m pytest tests/ -q  -> 78 passed
逐提交验证     -> git worktree + pytest 跑过分支上每个提交（曾发现一个"测试先于实现"的提交，
                  已重建历史修正；本轮又发现并修掉一处热路径性能回归）
```

### 9.5 目标完成情况复核（v1.5.6）

按"评估文档里的每个建议 → 代码现状"逐项复核（v1.5.6，`make ci` 全绿）：

| 目标 | 状态 | 证据 |
|---|---|---|
| Phase 0：CI、依赖锁定、版本单源、i18n/帧契约、性能基线、HIL 入口、四类守卫（架构/ID/文档/注册可达性） | **全部完成** | `make ci` 依次执行并通过 |
| Phase 1：P1-1…P1-11 | **全部完成** | 命令表、循环依赖 0、面板拆分、参数槽位、health/auto_ref 视图、会话协议、fatal、JSON 边界 |
| Phase 2：E-1…E-5 | **全部完成** | `ParamSpec`+`/api/schema`、能力表限值、会话策略、帧保留策略表、视图/页签/i18n 命名空间三个注册点 |
| Phase 3：前端解耦 | **全部完成** | 循环依赖 14→0、面板拆分、结果仓库、帧解析单测、画布 DPR |
| G-1/G-2/G-3 | **完成** | 性能基线、依赖锁定、`make hw-test` |
| §7 扩展点 E-1…E-5 | **完成** | 同上；新增 `check_registrations.py` 保证注册点可达 |
| 剩余未做项 | 见 §9.3 | 唯二有实际价值的是"假后端 + e2e 进 CI"与"Firefox e2e"，其余为受影响项或低收益优化 |

**本轮之后发生的两次真实故障**（评估/重构本身没有覆盖到的）：

1. **画布全空（用户报告"系统无法使用"）**：破环后 `render/spectrum.ts` 不再被任何模块 import，其模块级 `setRenderer(renderAll)` 未执行，`requestRender()` 空转。没有异常、没有 console 报错；所有既有测试都过，因为它们断言的是**代理量**（帧计数、dataset、控件值）。→ 规则：注册副作用必须由入口显式 import，并用 `tools/check_registrations.py` 守护；测试必须断言用户可见结果（e2e 现在数画布非透明像素）。
2. **三个 UI 缺陷**（RTA 中心单位/键盘失效、测量时未禁用瀑布、溢出告警不可见）：分别源于"字段键 `rta_center` 与输入框 id `input-rta-center` 不一致"、"测量面板与瀑布图抢同一个显示位"、"溢出时设备不发帧而告警只在帧循环里绘制"。→ 规则：跨模块的字符串标识必须成对解析（`inputForField/fieldForInput`）；一个显示位只能有一个所有者；状态变化要主动重绘，不能只依赖数据流。

这两次故障的教训已写入 `DEVELOPMENT.md`（开发指南）的"教训台账"，并按"每条规则都要有守卫或测试"的要求落到了代码/CI 中。

## 附录 A：度量数据

| 指标 | 值 |
|---|---|
| 后端 Python 行数 / 文件 | 5,296 / 26 |
| 前端源码行数 / 文件（不含测试） | 9,826 / 60 |
| 前端测试行数 / 文件 | 1,386 / 12（115 用例） |
| 后端测试行数 / 文件 | 1,299 / 11（87 用例） |
| 最长后端函数 | `web/ws.py:_dispatch` 313 行、`measurements/sdr.py:step` 193 行、`measurements/rta.py:_configure_locked` 170 行、`web/http_api.py:build_status` 146 行、`web/ws.py:_validate_command` 135 行 |
| 最长前端函数 | `core/ws.ts:connectWS` 301 行、`ui/controls.ts:bindActions` 232 行、`core/ws.ts:updateStatus` 170 行、`render/spectrum.ts:renderRta` 113 行 |
| 前端循环依赖 | 14 条 |
| 后端循环依赖 | 1 条（`web/http_api ⇄ web/ws`） |
| 跨模块导入边 | 274 条（75 个模块） |
| `store.ts` 导入者 / 可变导出 / setter | 33 / ~70 / 62 |
| `index.html` id / data-action | 206 / 112 |
| DOM 查询（字符串字面量） | 295 处 |
| i18n 键 | en 452 / zh 451（缺 1；首版报告的 402/325 是提取缺陷，§8.1-4） |
| 默认分支提交数 / 版本 | 221 / 1.5.5 |

## 附录 B：复现脚本

```bash
# 1) 全部门禁
python3 -m pytest tests/ -q && python3 -m ruff check web_sa tests tools
(cd frontend/modern && npx tsc --noEmit && npm test -- --reporter=dot)

# 2) 前端循环依赖 + 导入边
python3 - <<'PY'
import os, re, collections
root='frontend/modern/src'
files=[os.path.join(dp,fn) for dp,_,fns in os.walk(root) for fn in fns if fn.endswith(('.ts','.js'))]
g=collections.defaultdict(set)
def res(b,s):
    p=os.path.normpath(os.path.join(os.path.dirname(b),s))
    return next((c for c in (p+'.ts',p+'.js',os.path.join(p,'index.ts')) if os.path.exists(c)),None)
for f in files:
    for m in re.finditer(r"from\s+['\"](\.[^'\"]+)['\"]", open(f).read()):
        r=res(f,m.group(1))
        if r: g[f].add(r)
color={}; st=[]; cyc=[]
def dfs(n):
    color[n]=1; st.append(n)
    for m in sorted(g[n]):
        if color.get(m,0)==0: dfs(m)
        elif color.get(m)==1: cyc.append(st[st.index(m):]+[m])
    st.pop(); color[n]=2
for f in files:
    if color.get(f,0)==0: dfs(f)
print(len(files),'modules,',sum(len(v) for v in g.values()),'imports,',len({frozenset(c) for c in cyc}),'cycles')
for c in sorted({frozenset(c) for c in cyc}, key=len, reverse=True):
    print('  '+' -> '.join(os.path.relpath(x,root) for x in c))
PY

# 3) 最长函数
python3 - <<'PY'
import ast, os
for dp,_,fns in os.walk('web_sa'):
    for fn in fns:
        if fn.endswith('.py'):
            p=os.path.join(dp,fn); t=ast.parse(open(p).read())
            for n in ast.walk(t):
                if isinstance(n,(ast.FunctionDef,ast.AsyncFunctionDef)) and n.end_lineno-n.lineno+1>=100:
                    print(n.end_lineno-n.lineno+1, f'{p}:{n.lineno}', n.name)
PY

# 4) 未被其它模块引用的导出（前端 API 面）
#    见正文 P0-1 / P2-2 的脚本，原理：解析 export 名 + 统计 import/命名空间访问

# 5) 前端单测覆盖映射（61 个源码模块中哪些被测试直接引用）
python3 - <<'PY'
import os, re, collections
root='frontend/modern/src'; tdir=os.path.join(root,'__tests__')
covered=set()
for fn in os.listdir(tdir):
    if not fn.endswith('.ts'): continue
    for m in re.finditer(r"from\s+'(\.[^']+)'", open(os.path.join(tdir,fn)).read()):
        c=os.path.normpath(os.path.join(tdir,m.group(1)))
        for ext in ('','.ts','.js'):
            if os.path.exists(c+ext): covered.add(c+ext); break
srcs=[os.path.join(dp,fn) for dp,_,fns in os.walk(root) for fn in fns
      if fn.endswith(('.ts','.js')) and '__tests__' not in dp]
un=sorted(s for s in srcs if s not in covered)
print(f'{len(srcs)} modules, {len(srcs)-len(un)} covered, {len(un)} uncovered')
c=collections.Counter(os.path.dirname(os.path.relpath(s,root)) for s in un)
for d,n in sorted(c.items()): print(f'  {n:3d}  {d}/')
PY
```

## 附录 C：与既有待办的关系（重要）

根目录 `TODO-frontend-state.md`（27KB，本地私有）中"**仍未做：SWP/RTA 迁移（唯一剩余项）**"
一节已**过期**：代码中 `ui/freqState.ts`、`ui/swpState.ts`、`ui/refState.ts`、
`ui/displayRef.ts`、`ui/graphMode.ts` 均已使用 `core/params.ts`，对应提交
`83ed00c`（SWP 参数槽位化）与 `ee2f987`（graphMode/displayRef 重建）。
本报告以**代码现状**为准；建议按 P2-6 归档该文档，避免后续评估被误导。

**本节此前列出的两条"仍然成立的遗留项"也已完成**（v1.5.6 复核）：

| 当时的结论 | 现状 | 证据 |
|---|---|---|
| 触发/瀑布/测量结果仍是裸全局（P1-6） | **已完成** | 触发组 `ui/triggerState.ts`、瀑布组 `ui/waterfallState.ts`、显示组 `ui/displayState.ts` 均为 `createParam` 槽位（提交 `514c2b8`、`829907d`）；测量结果移入 `core/results.ts`（20 项数据 + 显式 setter） |
| RTA 侧仅剩触发参数组与部分 store 副本（P1-6 第 1 条） | **已完成** | `trigSource/trigLevel/trigEdge/trigPoi` 已槽位化，store 只 re-export；`store.ts` 的可变全局从 ~70 降到 37，且剩余项为运行态/数据/常量（连接状态、触发运行态、迹线/密度/限制线数据、模式状态机） |

**未完成项的唯一权威清单在 §9.3**（本附录不再单独维护，避免两份清单再次不同步）。
`store.ts` 中仍保留的**单写者客户端偏好**（`spanStepHz/spanStepAuto`、`dbPerDiv`、`levelUnit`、
`currentGapFill`、`units`、`harmValMode`、`peakListOn`、`peakThrUserSet`、`normRefWinUser`、
`valleySeqPos`）不属于 P1-6 针对的"多写者"问题，是否迁移见 §9.3 的说明。

