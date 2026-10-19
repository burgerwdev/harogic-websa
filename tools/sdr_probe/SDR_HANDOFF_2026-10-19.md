# SDR 静噪、SWT 与高带宽音频交接（2026-10-19）

## 接手先看

- 分支 `fix/review-1.8.16-20261018`；本轮提交修复静噪、扫频时间显示和 <=12.5 MHz 的音频吞吐。**25 MHz 连续音频未解决**，下一轮目标是原生自研 DDC/流水线 A/B；50 MHz 不在本轮承诺内。
- Pi Goal `mvezagtc-l92v6w` 已暂停，任务进度仍为 pending：`update_goal_task` 工具会报 `Use either updates or single-task fields, never both.`。用户要求工具出错后立即停下告知，不重试或绕过；仅用户 `/goal-resume` 后继续 Goal 生命周期。不要直接改 `.pi/goals/` 文件。
- 用户要求**保留 SDR 频谱原有 20 fps 上限**，不得为保音频降至 10 fps；注释/历史测试只作线索，实际代码和硬件测量为准。真实异机局域网尚未复测，CDP 限速只是模拟。
- 现场 WebSA 服务：`http://127.0.0.1:8080`，由 `./run.sh` / `./stop.sh` 管理。重启前核对现有会话；此前用户已允许必要的受控重启，但不可终止其浏览器。SAN-90 与 tinySA Ultra 同轴连接；测后 tinySA 必须 `output off`，恢复原始 SWP/SDR 参数。SAN-90 设备句柄只能由一个进程占用。

## 已验证改动

1. `web_sa/measurements/sdr.py` 的静噪取调谐频点附近的**AGC 前频谱峰值 dBm**，通过 `STATUS.sdr.channel_dbm` 和前端 `frontend/src/sdr/squelchLevel.ts` 同步给 WASM；Python 后备、WASM 均按该门限开关。WASM ABI 从 3 升 4，已重建 `frontend/public/dsp.wasm` 和 `wasm/dsp.artifact.json`。tinySA -40 dBm AM 峰约 -40.3 dBm、25 kHz 偏移 FM 峰约 -47.7 dBm：-60 门限开、-20 关；关 RF 后关；<=-100 默认常开。这里是 bin 峰值，**不是 FM 的积分总功率**。WASM/后备、STATUS 门控一致，FM 单声道硬件验证；立体声/RDS/DRM 用采样 fixture 回归，不得称为空口验证。
2. 设备 `EstimateMinSweepTime` 单位为秒，换算为毫秒的 `sweep_ms`，保留在 FREQ/POWR 帧头。发布器实测单步耗时 EMA 独立存为 `STATUS.sweep_ms_meas`。当前 SAN-90/SDK 在所测配置（含 manual 0.1 s）估算都为 **0**；SWT 因此用 `xx.x ms (实测)` 标记 EMA，设备以后返回非零估算则优先显示设备值。注意 EMA 是发布器采集/发布步骤耗时，不能宣称为设备扫描绝对周期。前端 `status.test.ts` 覆盖旧后端缺字段和两字段不混写。
3. `SdrSession.pacing()` 原用是否发布帧判定退避；IQBF 要积攒 8000 采样才发布，导致成功收到包仍睡 2 ms。现用 `_last_step_ok`，6.25 MHz 从约 29.4k/48.225k 恢复至 48.1k/48.225k 复采样/秒。12.5 MHz 每包仅约 1.04 ms，`web_sa/web/publisher.py` 在此档把**两次完整逐包 session.step**合到一次 executor 往返：48.1k/48.1k 复采样/秒，10 秒浏览器 0 underrun、0 IQ 丢序、约 210 频谱帧。厂商 DDC 仍每包调用一次。
4. Worklet 模拟缓冲预充/目标为 450/500 ms；重调谐 60 ms 快预充最多 300 ms，缺样本时按播放时钟退出；`sdrAudioWorklet.test.ts` 覆盖。没有改 IQBF 分帧 8000，也没有保留 10 fps 降帧。

## 25 MHz 剩余根因与证据

- `python3 -m tools.bench.sdr_raw_iqs_check --decimates 2 --seconds 3`（**WebSA 停止时独占硬件**）：原始 IQS 31.255/31.25 MSps、0 SDK 错包。USB/原始采集可满速。
- 在 WebSA 用单 Chromium、本机 25 MHz（decimate 2）时：基带仅约 25k/48k 复采样/秒、音频中断、3 次 SDK 超时及流恢复，频谱有时不足 20 fps。帧序号无缺口不代表音频样本率足够。
- 25 MHz 下 IQS 一包 16240 采样，期限约 **0.52 ms**；`WEBSA_SDR_TIME=1` 实测 WebSA 每包约 **0.82 ms**：取流约 0.28 ms，厂商 DDC 约 0.40 ms，基带/编码约 0.12 ms。即使取消线程往返，串行处理仍超预算。提高浏览器缓冲无法修复持续缺额。
- **禁止 `WEBSA_SDR_DDC_BATCH=2`**：它把两包交给一次厂商 DDC，而非发布器的两次逐包 step；A/B 期间 worker SIGSEGV（2026-10-19 18:12，ctypes 对象回收栈）。已撤掉临时变量并恢复正常服务；没有证明具体 native 越界点，不要重试。
- 下轮需要原生信道化器或安全并行架构的实测 A/B，目标 25 MHz 连续音频、保留 20 fps。不能先直接替换厂商 DDC：要在同一原始 IQ 录制输入下核对频率符号/相位连续、抽取样本数与时间戳、群延迟、抗混叠、AM/FM 单声道/立体声/RDS、DRM、FT8，以及重调谐/掉包恢复；同时测 CPU、包期限、长期 underrun。建议原实现与新路径可并列测试，确认后才切默认。多频点监听/录制/模式识别是后续需求，不作为本轮未验证功能宣称交付。

## LAN 限速：正确解读

- 原始 RTAF+IQBF 载荷测得约 876 kB/s（20 fps 时），但 WebSocket 已协商 `permessage-deflate`，**不能仅用未压缩字节数推断线上精确速率**。单实例 Chromium CDP 700 kB/s、80 ms 限速时，3.13 MHz 8 秒约 98 个前端频谱帧、2 次 underrun；2 MB/s、80 ms 时 160 帧/8 秒、0 underrun。网络是独立瓶颈，不靠降低频谱 fps 掩盖。
- 真实异机现状不明：服务当前只绑定 `127.0.0.1`，下轮先确认实际远程访问路径（反向代理、SSH 隧道等）及实测链路。勿把单机限速结果写成异机验收。

## 下轮执行顺序

1. 查 `git status --short --branch`、当前服务/浏览器/设备状态；阅读本文件、`tools/sdr_probe/FINDINGS.md` §20 与 `web_sa/demod/ddc.py`、`web_sa/measurements/sdr.py`、`web_sa/web/publisher.py`。避免无意覆盖当前分支改动。
2. 确认原始 IQS 25 MHz baseline（已有上述数据，不必重复长跑）；先明确新 DDC 的时间/精度验收，例如 25 MHz 下复采样交付接近标称、20 fps、0 持续 underrun 和 0 丢序，再用固定 IQ fixture 逐段比较厂商输出。测试异常立即退出、恢复服务。
3. 自研 DDC 若无法在 0.52 ms/包的预算内保证音质，需考虑捕获/信道化独立流水线与设备 SDK 的线程安全边界；不能并发调用同一 `libhtraapi` 句柄（曾导致崩溃）。不要以丢 IQ 包或降低频谱 fps 换达标数字。
4. 单 Chromium 测本机与合理 CDP 限速，收集 `sdrIq`（`dropped`、`dsp_underruns`、`dsp_fill_ms`、`dsp_playing`）、`STATUS.sdr.actual.ddc_delivery_rate`、`health` 和实际频谱帧数。真实异机若可用，再单独验收，不混作一组结果。

## 复验命令与风险

- 快速门禁：`python3 -m pytest -q tests/test_publisher.py tests/test_sdr.py tests/test_http_api.py`、`cd frontend && npx vitest run src/__tests__/status.test.ts src/__tests__/sdrAudioWorklet.test.ts src/__tests__/squelchLevel.test.ts src/__tests__/wasmPipeline.test.ts`、`python3 tools/checks/check_wasm_artifact.py`、`cd frontend && npm run build`。
- 单实例浏览器：`python3 tools/bench/sdr_network_diagnostics.py --decimates 8 4 --profiles local --seconds 8`；限速加 `--profiles limited --download-bytes 2000000`。脚本正常退出时恢复初始 SDR 参数和模式；**服务崩溃时 finally 无法连接恢复**，需人工核对/恢复。不要启动多个 Chromium 实例并行测。
- 射频验证：`python3 tools/bench/sdr_squelch_diagnostics.py --modulation am` 或 `--modulation fm`；结束检查 tinySA 输出关闭。设备独占探针前先 `./stop.sh`，之后 `./run.sh`。
- 完整并行 Rust + Vitest + pytest 曾使 3 个 CW 用例超过 5 秒，单独跑该文件 8/8 通过；**不要再次并行跑重套件**。`ruff check web_sa tests tools` 在未修改的 `tests/test_device_state.py` 仍有 2 条既存 lint 错误（I001、F401）；本轮涉及的 Python 路径单独 lint 已通过。测试只证明覆盖路径，硬件表现以实测为准。
