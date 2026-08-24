# legacy 前端 → 现代 TS 重构 留痕

## 目标
在 web_sa_release 内将 legacy 前端（app.js 2480 行 + index.html + style.css）
完整重构为现代 TypeScript 前端，**功能与效果完整复刻**，新增：
- **i18n**（中/英文切换）
- **主题切换**（dark/light；light 模式下 marker/迹线等需高对比度配色）

## 里程碑
- [x] 0. 用户已自行备份目录（web_sa_release.bak）
- [x] 1. 留痕文件 + legacy 功能清单
- [x] 2. 工程骨架 (frontend/modern: package.json/tsconfig/vite/index.html/style.css)
- [x] 3. i18n 字典 + 主题系统
- [x] 4. WS 协议层 + store
- [x] 5. 频谱渲染 (canvas)
- [x] 6. 控制面板 UI
- [x] 7. DSP 引擎移植
- [x] 8. 测量模式 + 参考时钟 + 归一化
- [x] 9. 构建 + 切换 + 验证(首轮)

## legacy 功能清单（app.js 2480 行, 模块化依据）
1. **工具/格式化**: fmtAxis/formatFreqHz/formatBWHz/parseFreqUnit/toUnit/单位组(Hz/kHz/MHz/GHz, s/ms/us, Hz..GHz)
2. **状态/WS**: send(obj)/CONNECT/STATUS(device_detail/has_docxo/ref_clk/preamp_actual/ifgain_actual/mode/caps)/二进制帧(FREQ/POWR)
3. **控制命令**: 中心/span、起始/停止、全频段(4.5G±4.49995G)、参考电平、dB/div、RBW/VBW/点数、杂散模式、窗口(0-4 FlatTop..Kaiser)、参考时钟(Int/Ext/ExtForce+输出)、增益(前置/IF)、偏移、gap填充
4. **迹线**: resampleTrace(保峰)/gapFill/completeEnvelope/processTraces(状态机)/平滑(MAXHOLD最大窗等)/trace mode(Normal/MAX_HOLD/MIN_HOLD/AVG)/3条迹线tab
5. **归一化**: normRefWindow/classifySource/buildReferenceTable/removeOutliers/cleanReference/fillSpurDips/normalizeActiveTrace(显示层变换, 钳0)
6. **渲染**: renderGrid/renderTraceLine/renderMarkersOnCanvas/drawDimLine/renderOSD(状态条)/render3dB/renderAll
7. **DSP 寻峰寻谷**: sgSmooth(2阶+梯度自适应)/parabolaFit/hasExcursion(双侧6dB)/findExtremesOrdered(峰3bin谷25bin凹陷合并重建为凹陷最低)/nextExtreme(频率方向遍历, 最近匹配±3bin)/Raw Anchor(非平滑)/Valley(显示全局最低+抛物线)
8. **3dB 测量**: measure3dB(带宽/中心/Q)
9. **谐波测量**: measureHarmonics(服务器逐谐波H1-H5)/renderHarmonics/renderHarmOverlay/表格
10. **幅度测量**: measureAmp/crossX/renderAmp
11. **相噪测量**: measPnmApply/onPnmResult/renderPnm(6档频偏100Hz-10MHz)/表格/平滑
12. **Pk List**: findPeaks(阈值)/noiseFloor/autoPeakThr(峰值-50, 用户锁定activeElement, 全关不更新)/updatePeakTable/renderPeakMarks(P1~Pn)
13. **Preset/测量模式**: presetAll(设备默认)/measToggle/3 tab(Amplitude/Harmonic/PhaseNoise)/exitMeasMode
14. **Marker 管理**: 4个marker/initMarkerTable/updateMarkerMode/selectMarker/autoTrackMarker/placeMarkerFromX/markerToCenter/setMarkerIdx(亚频点)
15. **UI 组**: toggleGroup/toggleAllGroups/syncToggleIcons

## 决策记录
- 技术栈: Vite + TypeScript(复用 web_sa_new 配置模式, 无框架原生 DOM)
- 构建产物: frontend/modern/dist, 后端 http_api 增加切换(默认 legacy 或 modern, 可配置)
- i18n: 内置字典 zh/en, data-i18n 属性 + JS 动态文本, 全局切换
- 主题: CSS 变量(canvas 颜色 JS 读变量), light 模式重建对比度配色
- 与原版一致性: 后端不动(仅 http_api 静态路由可切), WS 协议不变

## 风险/注意
- legacy 中 np.interp 升采样三角波问题(已在 legacy 修复: 后端不重采样) — 重构保持
- MAX_HOLD 平滑最大窗填平深谷 — Raw Anchor 修正(非平滑) — 保持
- 寻谷: 谷排序按平滑显示深度/位置用 Raw Anchor 原始值 — 保持 legacy 最终行为
- 窗口枚举: 与官方一致(FlatTop/Blackman_Nuttall/LowSideLobe/Rectangle/Kaiser)

## 验证记录 (2026-08-24)
- modern UI 启动: WEB_SA_UI=modern python3 -m web_sa.main; / 返回 modern dist/index.html
- 连接/状态/渲染/窗口选项/i18n中文/主题light/marker/寻峰(999.92MHz -21.32dBm) 全部通过, 零 JS 错误
- 切换: http_api UI_MODE env (legacy 默认 | modern); run.sh 未带 env 时为 legacy
- 截图: /tmp/modern_light.png (light 主题)

## 结构 (frontend/modern/src)
- core/ store(状态) ws(协议+STATUS) wsSend(独立send) fmt units i18n theme markerCommon
- dsp/ smooth(S-G) peaks(三级寻峰) traces(状态机) normalize(归一化)
- render/ spectrum(主渲染) plot infobar markerTable peaklist
- meas/ amplitude harmonic harmOverlay harmOverlay2 phaseNoise
- ui/ controls(data-action绑定+canvas交互) measure(测量状态机) traceOps normPub
- main.ts 入口(主题/语言持久化 localStorage)
