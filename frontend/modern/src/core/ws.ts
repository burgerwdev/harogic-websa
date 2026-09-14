// WebSocket protocol layer + STATUS handling
import { requestRender } from '../render/redraw';
import { updateFreqUIInputs } from '../ui/freqInputs';
import * as S from './store';
import { decodeFrame } from './frames';
import { t, hasKey } from './i18n';
import { updateInfoBar } from '../render/infobar';
import {
  syncRefClkOut,
  fillGnssDetail,
  syncGraphModeStatus,
  releaseGraphModePending,
  syncFrequencyEditorStatus,
  syncScaleButtons,
  syncSwpSpanStep,
  syncSdrPanel,
  currentGraphMode,
} from '../ui/controls';
import { invalidateAllTraces } from '../dsp/traces';
import { syncAvgUI, showNormalizeClearedHint } from '../ui/traceOps';
import { accumulateTrace } from '../dsp/accumulator';
import { pushRtaRow, waterfallRowWidth } from '../render/waterfall';
import { setWS } from './wsSend';
import { refreshRefClockHint } from './refclock';
import { retrackMarkers } from './markerCommon';
import { processTraces } from '../dsp/traces';
import { noteFrameArrived } from '../ui/triggerEvents';
import { evaluateSwpTrigger } from '../ui/swpTrigger';
import { onHarmResult } from '../meas/harmonic';
import { onPnmResult } from '../meas/phaseNoise';
import { percentileApprox, plausibleSpectrum } from '../dsp/stats';
import { alignToDisplayWindow } from '../dsp/grid';
import { getDisplayRef, setDisplayRef, noteDisplayRefReport } from '../ui/displayRef';
import { updateTrackingMarkers } from '../dsp/markerTracking';
import { refLevel } from '../ui/refState';
import { maybeRequestSdrFit, noteTraceObservation, postRefNotice, syncAutoScaleStatus }
  from '../ui/refAutoScale';
import { centerHz, spanHz, swpCenterHz, rtaCenterHz } from '../ui/freqState';
import {
  rbwMode, vbwMode, currentRBW, currentVBW, currentPoints, currentSpur,
} from '../ui/swpState';
import { rtaAmpBins, rtaFade, waterfallOn, wfPaused } from '../ui/waterfallState';
import { displayOffset, displayUnit } from '../ui/displayState';

/** The last (requested, reported) Ref pair announced, so the notice fires on a change only. */
let lastRefLimit: string | null = null;

/**
 * Report a reference level the device did not accept verbatim.
 *
 * `req` is what we asked the profile for, `actual` what the hardware programmed and echoed; the
 * difference is the device's own limit for its current front end, which the UI otherwise follows
 * silently (reported: "Ref 30 dBm jumps back to 27 after a few seconds - why?").
 */
function syncRefLimitNotice(s: any): void {
  const mode = String(s?.mode ?? '');
  if (mode === 'sdr') return;                       // the SDR display scale is client-side
  const reqRef = Number((mode === 'rta' ? s?.req?.rta : s?.req?.swp)?.ref);
  const actualRef = Number(s?.actual?.ref ?? s?.ref);
  const clamped = isFinite(reqRef) && isFinite(actualRef) && Math.abs(reqRef - actualRef) >= 1;
  const key = clamped ? `${reqRef}->${actualRef}` : null;
  if (key === lastRefLimit) return;
  lastRefLimit = key;
  if (clamped) postRefNotice(t('ref_limited', { ref: actualRef.toFixed(0) }), 5000);
}

function localizedError(msg: any): string {
  const code = String(msg?.code || '');
  const params: Record<string, string | number> = { ...(msg?.params || {}) };
  if (params.session) {
    params.session = t(String(params.session) === 'pnm' ? 'phase_noise' : 'harmonic');
  }
  const key = code ? `err_${code}` : '';
  const text = key && hasKey(key) ? t(key, params) : (msg?.msg || t('err_command'));
  return `${t('alert_device')}: ${text}`;
}

let ws: WebSocket | null = null;
let reconnectTimer: number | null = null;
let reconnectDelay = 1000;
let lastRtaProcess = 0;
let lastRender = 0;
let rtaFrames = 0;
let lastRtaInfoAt = 0;
let lastRtaStartHz = 0, lastRtaStopHz = 0;
let lastDensRef = 0, lastDensRange = 0;

// Re-initialise the SDR auto-scale (called when entering SDR).
let firstConnect = true;

export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null;
    connectWS();
  }, reconnectDelay);
  reconnectDelay = Math.min(10000, reconnectDelay * 2);
}

// Unusable frames are expected for a moment after a reconfiguration; dropping them for ever
// leaves a blank canvas with no explanation, so the drop is bounded and reported.
const RTA_BAD_MAX_FRAMES = 20;
const RTA_BAD_MAX_MS = 300;
let rtaBadFirst = 0;
let rtaBadCount = 0;

let lastOverflowWarning = false;

export function connectWS() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const queryToken = new URLSearchParams(location.search).get('token');
  if (queryToken) sessionStorage.setItem('web-sa-token', queryToken);
  const token = sessionStorage.getItem('web-sa-token');
  // The display connection carries no audio: SDR playback has its own `?audio=1` socket
  // inside the audio worker, so a busy main thread cannot starve it.
  const params = new URLSearchParams();
  if (token) params.set('token', token);
  params.set('noaudio', '1');
  ws = new WebSocket(`${protocol}//${location.host}/ws?${params.toString()}`);
  setWS(ws);   // Key: all commands (send) go through the unified wsSend exit, must be initialized
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    reconnectDelay = 1000;
    send({ cmd: 'STATUS' });
    // First load: force the backend back to standard sweep. A leftover RTA session
    // keeps pushing RTAF frames (no SWP data) while the UI defaults to std ->
    // blank spectrum until RTA is toggled twice.
    if (firstConnect) {
      firstConnect = false;
      // First load: restore saved mode (default std). A leftover RTA session on the
      // backend would otherwise push RTAF frames with no SWP data -> blank spectrum.
      const saved = localStorage.getItem('web-sa-mode');
      const wantMode = saved === 'rta' ? 'rta' : saved === 'sdr' ? 'sdr' : 'std';
      const wantRta = wantMode !== 'std';
      send({ cmd: 'SET_MODE', mode: wantMode });
      if (!wantRta) { S.setViewMode('std'); S.setRtaMode(false); }
      else { S.setViewMode('rta'); S.setRtaMode(true); }
    }
  };
  ws.onerror = () => ws?.close();
  ws.onclose = () => {
    S.setDeviceConnected(false);
    releaseGraphModePending();
    updateInfoBar();
    setWS(null);
    ws = null;
    scheduleReconnect();
  };
  ws.onmessage = (event: MessageEvent) => {
    if (typeof event.data === 'string') {
      try {
        const msg = JSON.parse(event.data as string);
        if (msg.cmd === 'STATUS') updateStatus(msg);
        else if (msg.cmd === 'HARM') onHarmResult(msg.list);
        else if (msg.cmd === 'PNM') onPnmResult(msg);
        else if (msg.cmd === 'ERROR') {
          releaseGraphModePending();
          alert(localizedError(msg));
        }
      } catch (error) {
        console.error('Invalid WebSocket JSON message', error);
      }
      return;
    }
    if (!(event.data instanceof ArrayBuffer)) return;
    const frame = decodeFrame(event.data);
    if (frame === null || frame.kind === 'audio') return;   // audio has its own connection
    const { points } = frame;
    if (frame.kind !== 'rta' && frame.sweepMs > 0 && frame.sweepMs !== S.sweepMs) {
      S.setSweepMs(frame.sweepMs);
      updateInfoBar();
    }
    if (points < 2) return;
    if (S.swpHold) return;               // SWP software capture: hold the swept display
    if (frame.kind === 'rta') {
      const processAt = performance.now();
      if (processAt - lastRtaProcess < 30) return;
      lastRtaProcess = processAt;
      // RTA frame layout lives in core/frames.ts (magic + ver + pts + wfLen + maxD +
      // startHz, then freq(f8) + spec(f4) + wfRow(u2) + stopHz(f8)); the decoder has
      // already validated every length, so nothing here has to re-derive strides.
      const { version: ver, maxDensity, startHz, stopHz } = frame;
      const capFreq = frame.freq;
      const capSpec = frame.spec;
      const wfRow = frame.wfRow;
      const pts = frame.points;
      // The frame header is the DISPLAY window, the freq array the CAPTURE grid. In SDR the
      // two differ when a hardware offset moved the capture centre; rebin to the display
      // window so the user's centre is at the canvas centre and the offset edge is a gap.
      const { freq, spec, shifted } = alignToDisplayWindow(capFreq, capSpec, startHz, stopHz);
      {
        // Debug/verification aid: the windows the renderer actually uses (e2e reads it).
        const cvW = document.getElementById('spectrum');
        if (cvW && (shifted || currentGraphMode() === 'sdr')) {
          cvW.dataset.sdrWindow = JSON.stringify({
            lo: startHz, hi: stopHz,
            capLo: Number(capFreq[0]), capHi: Number(capFreq[pts - 1]), shifted,
          });
        }
      }
      const plausible = plausibleSpectrum(spec);
      if (plausible) {
        rtaBadCount = 0;
        if (S.badData) S.setBadData(false);
      } else {
        if (rtaBadCount === 0) rtaBadFirst = performance.now();
        rtaBadCount++;
      }
      const settleOver = rtaBadCount > RTA_BAD_MAX_FRAMES || performance.now() - rtaBadFirst > RTA_BAD_MAX_MS;
      if (!plausible && !settleOver) return;    // settle window only: drop quietly
      if (!plausible) S.setBadData(true);       // past it, show the data and say so
      // SDR: the swept reference level is meaningless here (often 0 dBm against a -100 dBm
      // noise floor), so entering the mode asks the backend for one fit; the client applies the
      // reported target to the display scale (see ui/refAutoScale.ts). Nothing runs unattended.
      if (currentGraphMode() === 'sdr') maybeRequestSdrFit();
      // The RTA frequency window (center/span) changed -> every accumulation (probability
      // density, per-trace displays, waterfall rows) lives on the OLD frequency axis and
      // must be reset, otherwise stale dots/traces linger at wrong frequencies.
      const axisChanged = lastRtaStartHz === 0
        || Math.abs(startHz - lastRtaStartHz) > 0.5
        || Math.abs(stopHz - lastRtaStopHz) > 0.5;
      if (axisChanged) {
        if (S.rtaDensity2d) S.rtaDensity2d!.fill(0);
        for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
        for (let ti = 0; ti < S.rtaAvgN.length; ti++) { S.rtaAvgN[ti] = 0; S.rtaAvgSum[ti] = null; S.rtaDone[ti] = false; }
        S.resetWaterfall();
      }
      lastRtaStartHz = startHz;
      lastRtaStopHz = stopHz;
      S.setRtaData({ ver, freq, spec, wfRow, maxDensity, startHz, stopHz });
      {
        // Debug/verification aid: proves a frame was actually decoded and handed to the
        // renderer, which the mode flag alone does not (e2e reads it).
        const cvF = document.getElementById('spectrum');
        if (cvF) cvF.dataset.rtaFrames = String(rtaFrames++);
      }
      // Refresh info-bar (BW/RBW follow the frame's start/stop) at a throttled rate
      const _nowU = performance.now();
      if (_nowU - lastRtaInfoAt > 400) {
        lastRtaInfoAt = _nowU;
        updateInfoBar();
      }
      // RTA mode has no FREQ frames; sync the frequency axis so markers map correctly
      S.setFreqArray(freq);
      if (axisChanged) retrackMarkers();
      // 2D probability density (freq x amplitude bins): points along the signal trace
      // accumulate and fade - official-style density dots, not full columns.
      // The bin grid is anchored to the CURRENT display window (refTop..refTop-range):
      // if the user changes ref level or scale (dbPerDiv) the grid moves with the trace,
      // so density and trace never drift apart. A window change rebuilds the grid.
      const dispRange = S.totalDivs * S.dbPerDiv;
      // Slot reads are cheap but not free: hoist them out of the per-bin loops below
      // (bins * points iterations per frame). Reading them inside the loop made the main
      // thread miss the 1 Hz STATUS cadence and stalled the UI during SDR/RTA entry.
      const bins = rtaAmpBins.get();
      const fade = rtaFade.get();
      const dB_PER_BIN = dispRange / bins;
      const refTop = getDisplayRef();
      if (lastDensRef !== refTop || lastDensRange !== dispRange) {
        if (S.rtaDensity2d) S.rtaDensity2d!.fill(0);
        lastDensRef = refTop; lastDensRange = dispRange;
      }
      const len2 = spec.length * bins;
      const floorN = percentileApprox(spec, 0.3);
      // Amplitude-graded weight: how far a point sits above the noise floor decides how
      // strongly it accumulates. Weak signals (>3 dB) still leave a light density cloud
      // so the density map covers the whole trace; the floor ripple itself stays out.
      const accW = (relDb: number): number => {
        if (relDb < 3) return 0;
        if (relDb >= 25) return 1;
        return 0.25 + 0.75 * ((relDb - 3) / 22);
      };
      const pushDensity = (nd: Float32Array, i: number, _relDb: number, w: number) => {
        const b = Math.max(0, Math.min(bins - 1, Math.round((refTop - spec[i]) / dB_PER_BIN)));
        const o = i * bins;
        const bump = (bin: number, v: number) => {
          if (bin < 0 || bin >= bins) return;
          const k = o + bin;
          nd[k] += v;
          if (nd[k] > 40) nd[k] = 40;
        };
        const c = 3.5 * w;            // peak bin weight scales with signal strength
        bump(b, c);
        bump(b - 1, 2 * w);
        bump(b + 1, 2 * w);
        bump(b - 2, 1 * w);
        bump(b + 2, 1 * w);
      };
      if (!S.rtaDensity2d || S.rtaDensity2d!.length !== len2) {
        const nd = new Float32Array(len2);
        for (let i = 0; i < spec.length; i++) {
          const w = accW(spec[i] - floorN);
          if (w <= 0) continue;
          pushDensity(nd, i, spec[i] - floorN, w);
        }
        S.setRtaDensity2d(nd);
      } else {
        const nd = S.rtaDensity2d!;
        for (let i = 0; i < spec.length; i++) {
          for (let b = 0; b < bins; b++) {
            const v = nd![i * bins + b] * fade;
            nd![i * bins + b] = v > 0.05 ? v : 0;
          }
          const w = accW(spec[i] - floorN);
          if (w <= 0) continue;
          pushDensity(nd, i, spec[i] - floorN, w);
        }
      }

      // Per-trace accumulation (multi-trace like the official SW): each enabled trace
      // accumulates its own RTA display according to its mode.
      // RTA reuses the shared accumulator (single semantics with SWP). rtaDisplays stays
      // the storage so rendering, markers and waterfall are untouched.
      S.traces.forEach((tr, ti) => {
        if (tr.mode === 'OFF') return;
        const shim = {
          id: ti + 1, mode: tr.mode, prevMode: tr.prevMode,
          raw: null, powers: S.rtaDisplays[ti], avgSum: S.rtaAvgSum[ti],
          avgCount: S.rtaAvgN[ti], avgTarget: tr.avgTargetRta ?? 16, done: S.rtaDone[ti],
          reference: null, isNormalized: false,
        } as unknown as S.TraceState;
        accumulateTrace(shim, spec);
        S.rtaDisplays[ti] = shim.powers;
        S.rtaAvgSum[ti] = shim.avgSum;
        S.rtaAvgN[ti] = shim.avgCount;
        S.rtaDone[ti] = shim.done;
      });
      updateTrackingMarkers();
      if (waterfallOn.get() && S.rtaMode && !wfPaused.get()) {
        // bitmap rows are often all-zero; derive waterfall row from the live trace
        pushRtaRow(spec, waterfallRowWidth(), 100);  // same width as the swept path (one peak-hold stage)
      }
      const el = document.getElementById('info-pts');
      if (el) el.innerText = String(pts);
      noteFrameArrived();                    // armed: this packet IS the capture
      // RTA data arrived -> redraw at ~60fps (16ms throttle)
      const now = performance.now();
      if (now - lastRender >= 16) {
        lastRender = now;
        requestRender();
      }
      return;
    }
    if (frame.kind === 'freq') {
      S.setFreqArray(frame.freq);
      S.setFreqVersion(frame.version);
      const el = document.getElementById('info-pts');
      if (el) el.innerText = String(points);
      retrackMarkers();
    } else if (frame.kind === 'powr') {
      if (frame.version !== S.freqVersion) return;
      processTraces(frame.power);
      if (!S.rtaMode) evaluateSwpTrigger();  // software level trigger on consecutive sweeps
      noteFrameArrived();                    // armed: this trace IS the capture
      const now = performance.now();
      if (now - lastRender >= 33) {
        lastRender = now;
        requestRender();
      }
    }
  };
}

export function updateStatus(s: any) {
  if (!s || !s.req || !s.actual) return;
  if (s.caps) S.setFrequencyLimits(Number(s.caps.fmin), Number(s.caps.fmax));
  // STATUS top-level fields are the effective values for the active hardware mode.
  const isRtaStatus = s.mode === 'rta';
  if (s.req.rta?.center > 0) rtaCenterHz.confirm(Number(s.req.rta.center));
  centerHz.confirm(Number(s.center));
  if (s.mode !== 'rta' && s.mode !== 'sdr') swpCenterHz.confirm(Number(s.center));
  // -12 = APIRETVAL_WARNING_IFOverflow: the IF saturates when Ref is set low (gain rises as
  // Ref falls) and the device then stops delivering frames, so the display looks frozen.
  // The vendor's remedy is to RAISE the reference level. Shown in the canvas warning stack
  // (top-right) and as a pulsing outline on the Ref control. Cleared by a good frame.
  {
    const over = Number(s.status_warning) === -12;
    const refEl = document.getElementById('input-ref') as HTMLInputElement | null;
    if (refEl) {
      refEl.classList.toggle('ref-warn', over);
      refEl.title = over ? t('if_overflow_hint') : '';
    }
    S.setStatusWarnings(over ? ['!' + t('if_overflow_short'), '!' + t('if_overflow_hint')] : []);
  // The warning must be visible even though an overflowing IF stops sending frames: without
  // this repaint the canvas kept the previous pass (no warning), and it only appeared for one
  // frame after Ref was raised again (the user saw exactly that flash).
  if (over !== lastOverflowWarning) {
    lastOverflowWarning = over;
    requestRender();
  }
  }
  spanHz.confirm(Number(s.span));
  refLevel.confirm(Number(s.ref));
  S.setConfigVersion(Number(s.config_version) || 0);
  currentRBW.confirm(Number(s.rbw));
  currentVBW.confirm(Number(s.vbw));
  rbwMode.confirm(s.rbw_mode);
  vbwMode.confirm(s.vbw_mode);
  currentPoints.confirm(Number(s.points) || Number(s.req.swp?.points) || 1001);
  currentSpur.confirm(s.req.swp?.spur || s.spur || 'bypass');
  S.setSweepMs(s.sweep_ms || 0);
  S.setDeviceConnected(!!s.connected);
  syncGraphModeStatus(s.mode);
  syncFrequencyEditorStatus(s.response_to, S.configVersion);
  syncAvgUI();
  syncSwpSpanStep(Number(s.req.swp?.span) || spanHz.get());

  // The harmonic measurement retunes the device to each harmonic internally; the STATUS
  // centre/span those retunes report are not the display window, so they must not wipe the
  // trace (it made the canvas flash blank once per harmonic sequence).
  const harmonicMeasuring = S.measOn && S.viewMode === 'harm';
  const measKey = `${centerHz.get()}|${spanHz.get()}|${currentPoints.get()}|${currentRBW.get()}|${rbwMode.get()}|${s.window}`;
  if (measKey !== S.lastMeasKey) {
    S.setLastMeasKey(measKey);
    if (!harmonicMeasuring) {
      const hadNormalization = S.traces.some(trace => trace.isNormalized && trace.reference);
      invalidateAllTraces();
      if (hadNormalization) showNormalizeClearedHint();
    }
  }
  noteDisplayRefReport(Number(s.ref));
  if (displayUnit.get() !== 'dB') setDisplayRef('mode', refLevel.get());
  syncScaleButtons();

  const frequencyCommitted = s.response_to === 'SET_FREQ' || s.response_to === 'SET_RTA';
  updateFreqUIInputs(frequencyCommitted);
  const cur = refLevel.get();
  const refText = Number.isInteger(cur) ? cur.toFixed(0) : cur.toFixed(1);
  // The box always holds the reference LEVEL in dBm - it is what the Set button sends and what
  // the device reports. It used to be pinned to '0' in dB (relative) display mode, which made a
  // background change invisible (and made Set send 0 dBm); the relative axis top is a rendering
  // detail, not a different reference.
  setInput('input-ref', refText);
  const refInput = document.getElementById('input-ref') as HTMLInputElement | null;
  const refSet = document.getElementById('btn-ref-set') as HTMLButtonElement | null;
  // Nothing locks the reference any more: Auto Scale is a one-shot action, so the input, the
  // Set button and the step arrows stay usable and only their pending state is shown.
  if (refInput) refInput.disabled = false;
  if (refSet) refSet.disabled = false;
  const refDown = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  const refUp = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (refDown) {
    refDown.disabled = currentGraphMode() !== 'sdr' && cur <= -50;
    refDown.title = t('ref_down');
  }
  if (refUp) {
    refUp.disabled = currentGraphMode() !== 'sdr' && cur >= 30;
    refUp.title = t('ref_up');
  }
  // Auto Scale feedback: the button glows while a fit is in flight (the backend reports it via
  // auto_ref.adjusting, and the client keeps its own fallback timer), then reports the result.
  syncAutoScaleStatus(s);
  // A refusal notice describes the trace in front of the user: withdraw it the moment the trace
  // no longer matches (first frame arrived, or the peak moved away from the decision's basis).
  noteTraceObservation(s.auto_ref);
  // The device may clamp the reference (its own maximum depends on the attenuation/IF-gain it
  // picks: requesting +30 dBm on the SAN-90 comes back as +27). Say so instead of letting the
  // number change on its own.
  syncRefLimitNotice(s);
  setInput('input-points', String(currentPoints.get()));
  setSelect('select-rbw-mode', rbwMode.get());
  setSelect('select-vbw-mode', vbwMode.get());
  setSelect('select-spur', currentSpur.get());
  setSelect('select-detector', s.detector || 'auto');
  const wsel = document.getElementById('select-window') as HTMLSelectElement;
  if (wsel && document.activeElement !== wsel && s.window != null) wsel.value = String(s.window);
  const sweepSelect = document.getElementById('select-sweep-mode') as HTMLSelectElement | null;
  if (sweepSelect && document.activeElement !== sweepSelect) {
    sweepSelect.value = String(s.sweep_time_mode ?? 0);
  }
  if (isRtaStatus) {
    const spanSelect = document.getElementById('select-rta-span') as HTMLSelectElement | null;
    if (spanSelect && document.activeElement !== spanSelect) {
      const options = [...spanSelect.options];
      const nearest = options.reduce((best, option) =>
        Math.abs(Number(option.value) - spanHz.get()) < Math.abs(Number(best.value) - spanHz.get())
          ? option : best, options[0]);
      if (nearest) spanSelect.value = nearest.value;
    }
  }

  const dd = s.device_detail;
  if (dd) {
    const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
    set('dev-model', t('model') + ' ' + dd.model);
    set('dev-uid', dd.uid);
    set('dev-hw', dd.hw);
    set('dev-mfw', dd.mfw);
    set('dev-ffw', dd.ffw);
    set('dev-bus', dd.bus_speed + ' / v' + dd.bus_ver);
    set('dev-api', 'v' + dd.api_ver);
    set('dev-warn', dd.warnings);
  }
  S.setLastGnss(s.gnss);
  const g = document.getElementById('info-gnss');
  if (g) {
    // The top bar shows only the locked state (details such as satellite count live in the detail popover)
    if (s.gnss && s.gnss.lock) { g.textContent = t('status_locked'); g.style.color = 'var(--dot-on)'; }
    else { g.textContent = t('status_nolock'); g.style.color = 'var(--dot-off)'; }
    // Update the detail popover synchronously (if open)
    const pop = document.getElementById('gnss-popover');
    if (pop && pop.style.display !== 'none') fillGnssDetail();
  }
  const rc = document.getElementById('select-refclk') as HTMLSelectElement;
  syncRefClkOut(s);
  refreshRefClockHint(s, s.response_to);
  if (rc) {
    const hasOpt = rc.querySelector('option[value="premium"]');
    if (s.has_docxo && !hasOpt) {
      const o = document.createElement('option');
      o.value = 'premium'; o.textContent = t('docxo_premium');
      rc.appendChild(o);
    } else if (!s.has_docxo && hasOpt) {
      hasOpt.remove();
    }
    if (document.activeElement !== rc) rc.value = s.ref_clock;
  }
  if (s.amp) {
    setSelect('select-atten', String(s.amp.atten));
    setSelect('select-preamp', String(s.amp.preamp));
    setSelect('select-ifgain', String(s.amp.ifgain));
    setSelect('select-gainstrategy', String(s.amp.gain_strategy));
    const ca = document.getElementById('cur-atten');
    if (ca) ca.textContent = s.amp.atten_actual >= 0 ? s.amp.atten_actual + ' dB' : '';
    const cp = document.getElementById('cur-preamp');
    if (cp) cp.textContent = s.amp.preamp_actual === 1 ? t('off') : (s.amp.preamp_actual === 0 ? t('on') : '');
    const cg = document.getElementById('cur-ifgain');
    if (cg) cg.textContent = s.amp.ifgain_actual != null ? 'L' + s.amp.ifgain_actual : '';
    // With a manual channel attenuation the device resolves the preamplifier itself
    // (measured: it reports ForcedOff), so the Auto/Off choice has no effect. Say so
    // instead of silently ignoring the selector.
    const preampSel = document.getElementById('select-preamp') as HTMLSelectElement | null;
    if (preampSel) {
      const manualAtten = String(s.amp.atten) !== '-1';
      preampSel.disabled = manualAtten;
      preampSel.title = manualAtten ? t('preamp_manual_atten') : t('tip_select-preamp');
    }
  }
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of && document.activeElement !== of && Math.abs(parseFloat(of.value) - displayOffset.get()) > 0.01)
    of.value = displayOffset.get().toFixed(1);
  const gf = document.getElementById('btn-gapfill');
  if (gf) {
    gf.textContent = S.currentGapFill ? t('on') : t('off');
    gf.classList.toggle('active', !!S.currentGapFill);
  }
  updateInfoBar();
  syncSdrPanel(s);   // last: in SDR it owns the shared Ref widgets
}

function setInput(id: string, v: string, force = false) {
  const el = document.getElementById(id) as HTMLInputElement;
  if (el && (force || document.activeElement !== el)) el.value = v;
}
function setSelect(id: string, v: string) {
  const el = document.getElementById(id) as HTMLSelectElement;
  if (el && document.activeElement !== el) el.value = v;
}

// Sync frequency input fields
