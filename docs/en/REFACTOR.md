# legacy → Modern TS Refactor Log

## Goal
Fully refactor the legacy frontend (app.js 2480 lines + index.html + style.css)
into a modern TypeScript frontend with **feature/visual parity**, adding:
- **i18n** (EN/ZH switching)
- **Theme switching** (dark/light; light mode needs high-contrast colors for marker/traces)

## Milestones
- [x] 0. User backed up the directory (web_sa_release.bak)
- [x] 1. Log file + legacy feature inventory
- [x] 2. Project skeleton (frontend/modern: package.json/tsconfig/vite/index.html/style.css)
- [x] 3. i18n dictionary + theme system
- [x] 4. WS protocol layer + store
- [x] 5. Spectrum rendering (canvas)
- [x] 6. Control panel UI
- [x] 7. DSP engine port
- [x] 8. Measurement modes + reference clock + normalization
- [x] 9. Build + switch + validation (first round)

## legacy Feature Inventory (app.js 2480 lines, modularization basis)
1. **Utils/format**: fmtAxis/formatFreqHz/formatBWHz/parseFreqUnit/toUnit/unit groups (Hz/kHz/MHz/GHz)
2. **State/WS**: send(obj)/CONNECT/STATUS (device_detail/has_docxo/ref_clk/preamp_actual/ifgain_actual/mode/caps)/binary frames (FREQ/POWR)
3. **Control commands**: center/span, start/stop, full span (4.5G±4.49995G), ref level, dB/div, RBW/VBW/points, spur mode, windows (0-4 FlatTop..Kaiser), reference clock (Int/Ext/ExtForce+output), gain (preamp/IF), offset, gap fill
4. **Traces**: resampleTrace (peak-preserving)/gapFill/completeEnvelope/processTraces (state machine)/smoothing (MAXHOLD max-window etc.)/trace modes (Normal/MAX_HOLD/MIN_HOLD/AVG)/4 trace tabs
5. **Normalization**: normRefWindow/classifySource/buildReferenceTable/removeOutliers/cleanReference/fillSpurDips/normalizeActiveTrace (display-layer transform, clamp 0)
6. **Rendering**: renderGrid/renderTraceLine/renderMarkersOnCanvas/drawDimLine/renderOSD/render3dB/renderAll
7. **DSP peak/valley**: sgSmooth (2nd order + gradient-adaptive)/parabolaFit/hasExcursion (both-side 6dB)/findExtremesOrdered (peaks 3bin valleys 25bin depression merge rebuilt as minimum)/nextExtreme (frequency traversal, nearest ±3bin)/Raw Anchor (unsmoothed)/Valley (display global minimum + parabola)
8. **3dB measurement**: measure3dB (BW/center/Q)
9. **Harmonic measurement**: measureHarmonics (server auto-tune H1-H5)/renderHarmonics/renderHarmOverlay/table
10. **Amplitude measurement**: measureAmp/crossX/renderAmp
11. **Phase noise**: measPnmApply/onPnmResult/renderPnm (6 offsets 100Hz-10MHz)/table/smoothing
12. **Pk List**: findPeaks (threshold)/noiseFloor/autoPeakThr (peak-50, user lock activeElement, no update when all off)/updatePeakTable/renderPeakMarks (P1~Pn)
13. **Preset/measure modes**: presetAll (device defaults)/measToggle/3 tabs (Amplitude/Harmonic/PhaseNoise)/exitMeasMode
14. **Marker management**: 4 markers/initMarkerTable/updateMarkerMode/selectMarker/autoTrackMarker/placeMarkerFromX/markerToCenter/setMarkerIdx (sub-bin)
15. **UI groups**: toggleGroup/toggleAllGroups/syncToggleIcons

## Decisions
- Stack: Vite + TypeScript (based on web_sa_new config pattern, framework-less native DOM)
- Build output: frontend/modern/dist; backend http_api added UI switch (default legacy or modern, configurable)
- i18n: built-in dict zh/en, data-i18n attributes + JS dynamic text, global switch
- Theme: CSS variables (canvas colors read via JS), light mode rebuilt contrast palette
- Legacy parity: backend untouched (only http_api static routes switched), WS protocol unchanged

## Risks/Notes
- legacy np.interp upsampling triangle-wave issue (fixed in legacy: backend does not resample) — preserved in refactor
- MAX_HOLD smoothing max-window flattens deep notches — Raw Anchor correction (unsmoothed) — preserved
- Valley: sort by smoothed display depth / position uses Raw Anchor raw values — legacy final behavior preserved
- Window enum: matches official (FlatTop/Blackman_Nuttall/LowSideLobe/Rectangle/Kaiser)

## Validation Record (2026-08-24)
- modern UI start: WEB_SA_UI=modern python3 -m web_sa.main; / returns modern dist/index.html
- Connect/status/render/window options/i18n ZH/theme light/marker/peak (999.92MHz -21.32dBm) all passed, zero JS errors
- Switch: http_api UI_MODE env (legacy default | modern); run.sh without env is legacy
- Screenshot: /tmp/modern_light.png (light theme)

## Structure (frontend/modern/src)
- core/ store (state) ws (protocol+STATUS) wsSend (standalone send) fmt units i18n theme markerCommon
- dsp/ smooth (S-G) peaks (3-stage) traces (state machine) normalize
- render/ spectrum (main render) plot infobar markerTable peaklist
- meas/ amplitude harmonic harmOverlay harmOverlay2 phaseNoise
- ui/ controls (data-action binding + canvas interaction) measure (measure state machine) traceOps normPub
- main.ts entry (theme/lang persisted via localStorage)


## Modules added since v1.2.0

| File | Responsibility |
|---|---|
| `src/dsp/limits.ts` | limit interpolation, per-bin evaluation, violation runs (pure, unit tested) |
| `src/dsp/channel.ts` | band power, carrier-centred OBW, ACPR (pure, unit tested) |
| `src/dsp/levelCross.ts` | swept-mode level trigger: threshold crossing between two sweeps (pure, unit tested) |
| `src/core/level.ts` | amplitude units (dBm/dBmV/dBuV/dBV) and external gain/loss conversion |
| `src/ui/limits.ts` | limit panel, persistence, violation CSV |
| `src/ui/exportImage.ts` | PNG snapshot (acquisition header plus a bottom-right timestamp) |
| `src/ui/controlRail.ts` / `src/ui/railMath.ts` | jump rail and its scroll-spy maths |
| `src/ui/trigger.ts` / `src/ui/triggerEvents.ts` | trigger panel, status chip, RTA hit detection from the frame path |
| `src/ui/keypad.ts` | Virtual keypad: unit keys (reusing `UNIT_OPTIONS`), `data-keypad="text"` list entry with caret editing, dragging, `localStorage` toggle |
| `src/ui/swpTrigger.ts` | swept-mode software level trigger engine |
