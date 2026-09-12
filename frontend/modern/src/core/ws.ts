// WebSocket protocol layer + STATUS handling
import * as S from './store';
import { formatBWHz, formatFreqHz, fmtAxis } from './fmt';
import { toUnit } from './units';
import { t, hasKey } from './i18n';
import { updateInfoBar } from '../render/infobar';
import {
  syncRefClkOut,
  fillGnssDetail,
  syncGraphModeStatus,
  releaseGraphModePending,
  syncFrequencyEditorStatus,
  syncRefLevelStatus,
  syncScaleButtons,
  syncSwpSpanStep,
  syncSdrPanel,
  currentGraphMode,
} from '../ui/controls';
import { invalidateAllTraces } from '../dsp/traces';
import { syncAvgUI, showNormalizeClearedHint } from '../ui/traceOps';
import { accumulateTrace } from '../dsp/accumulator';
import { pushRtaRow, pushSwpRow, waterfallRowWidth } from '../render/waterfall';
import { setWS } from './wsSend';
import { refreshRefClockHint } from './refclock';
import { retrackMarkers } from './markerCommon';
import { processTraces } from '../dsp/traces';
import { noteFrameArrived } from '../ui/triggerEvents';
import { evaluateSwpTrigger } from '../ui/swpTrigger';
import { renderAll } from '../render/spectrum';
import { onHarmResult } from '../meas/harmonic';
import { onPnmResult } from '../meas/phaseNoise';
import { updateNormalizeStatusUI } from '../dsp/normalize';
import { percentileApprox, plausibleSpectrum } from '../dsp/stats';
import { updateTrackingMarkers } from '../dsp/markerTracking';
import { pushSdrAudio } from '../audio/sdrAudio';

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
let lastRtaInfoAt = 0;
let lastRtaStartHz = 0, lastRtaStopHz = 0;
let lastDensRef = 0, lastDensRange = 0;
let lastSdrRef = -999;
let sdrNoiseEma = -120;
let sdrPeakEma = -60;
let lastSdrAutoAt = 0;

// Re-initialise the SDR auto-scale (called when entering SDR).
export function resetSdrAutoRef() {
  lastSdrRef = -999;
  sdrNoiseEma = -120;
  sdrPeakEma = -60;
  lastSdrAutoAt = 0;
}
let firstConnect = true;
let rtaAvgN = 0;

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

export function connectWS() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const queryToken = new URLSearchParams(location.search).get('token');
  if (queryToken) sessionStorage.setItem('web-sa-token', queryToken);
  const token = sessionStorage.getItem('web-sa-token');
  const query = token ? `?token=${encodeURIComponent(token)}` : '';
  ws = new WebSocket(`${protocol}//${location.host}/ws${query}`);
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
    if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 16) return;
    const view = new DataView(event.data, 0, 16);
    const magic = String.fromCharCode(view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
    if (magic === 'AUDF') {
      // SDR audio: magic(4) + seq(u32) + rate(u32) + samples(u32) + int16 PCM.
      // A zero sequence starts a new configuration and flushes buffered channel audio.
      const seq = view.getUint32(4, true);
      const rate = view.getUint32(8, true);
      const samples = view.getUint32(12, true);
      if (event.data.byteLength !== 16 + samples * 2) return;
      pushSdrAudio(event.data, 16, samples, rate, seq === 0);
      return;
    }
    const version = view.getUint32(4, true);
    const points = view.getUint32(8, true);
    const sweepMsHdr = view.getFloat32(12, true);
    if (magic !== 'RTAF' && sweepMsHdr > 0 && sweepMsHdr !== S.sweepMs) {
      S.setSweepMs(sweepMsHdr);
      updateInfoBar();
    }
    if (points < 2) return;
    if (S.swpHold) return;               // SWP software capture: hold the swept display
    if (magic === 'RTAF') {
      const processAt = performance.now();
      if (processAt - lastRtaProcess < 30) return;
      lastRtaProcess = processAt;
      // RTA 帧: magic(4) + hdr(ver u32, pts u32, wfLen u16, maxD u16, startHz f8 = 20B) → 24B 头(8 对齐)
      // 数据: freq(f8×pts) + spec(f4×pts) + wfRow(u2×wfLen) + stopHz(f8)
      const hdr = new DataView(event.data, 4, 20);
      const ver = hdr.getUint32(0, true);
      const pts = hdr.getUint32(4, true);
      const wfLen = hdr.getUint16(8, true);
      const maxDensity = hdr.getUint16(10, true);
      const startHz = hdr.getFloat64(12, true);
      const expectedBytes = 24 + pts * 8 + pts * 4 + wfLen * 2 + 8;
      if (pts < 2 || wfLen < 1 || event.data.byteLength !== expectedBytes) return;
      let off = 24;
      const freq = new Float64Array(event.data, off, pts); off += pts * 8;
      const spec = new Float32Array(event.data, off, pts); off += pts * 4;
      const wfRow = new Uint16Array(event.data, off, wfLen); off += wfLen * 2;
      const stopHz = new DataView(event.data, off, 8).getFloat64(0, true);
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
      // SDR: the SWP reference level is meaningless (often 0 dBm) and would squash a
      // -100 dBm noise floor onto the bottom edge. Auto-scale the display ref to the
      // frame peak (with a small hysteresis) so the signal is visible.
      if (currentGraphMode() === 'sdr' && S.sdrRefAuto) {
        let peak = -Infinity;
        for (let i = 0; i < spec.length; i++) {
          const v = spec[i];
          if (v > peak && isFinite(v)) peak = v;
        }
        if (isFinite(peak)) {
          const noise = percentileApprox(spec, 0.3);
          // Smooth both so a fading signal does not make the whole display jump.
          sdrNoiseEma = sdrNoiseEma < -119 ? noise : sdrNoiseEma * 0.9 + noise * 0.1;
          sdrPeakEma = sdrPeakEma < -119 ? peak : sdrPeakEma * 0.75 + peak * 0.25;
          const now2 = performance.now();
          if (now2 - lastSdrAutoAt > 400) {
            const range = S.totalDivs * S.dbPerDiv;
            // Noise floor ~8 dB above the bottom; never clip the peak (>=10 dB headroom).
            // Noise floor ~8 dB above the bottom; never clip the peak (>=10 dB headroom).
            let ref = Math.max(sdrNoiseEma + range - 8, sdrPeakEma + 10);
            ref = Math.min(40, Math.max(-160, Math.ceil(ref / 5) * 5));
            if (Math.abs(ref - lastSdrRef) >= 3) {
              lastSdrRef = ref;
              S.setDisplayRef(ref);
              lastSdrAutoAt = now2;
              const cv = document.getElementById('spectrum');
              if (cv) cv.dataset.sdrRef = String(ref);   // debug/verification aid
            }
          }
        }
      }
      // The RTA frequency window (center/span) changed -> every accumulation (probability
      // density, per-trace displays, waterfall rows) lives on the OLD frequency axis and
      // must be reset, otherwise stale dots/traces linger at wrong frequencies.
      const axisChanged = lastRtaStartHz === 0
        || Math.abs(startHz - lastRtaStartHz) > 0.5
        || Math.abs(stopHz - lastRtaStopHz) > 0.5;
      if (axisChanged) {
        if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
        for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
        for (let ti = 0; ti < S.rtaAvgN.length; ti++) { S.rtaAvgN[ti] = 0; S.rtaAvgSum[ti] = null; S.rtaDone[ti] = false; }
        S.resetWaterfall();
      }
      lastRtaStartHz = startHz;
      lastRtaStopHz = stopHz;
      S.setRtaData({ ver, freq, spec, wfRow, maxDensity, startHz, stopHz });
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
      const dB_PER_BIN = dispRange / S.RTA_AMP_BINS;
      const refTop = S.displayRef;
      if (lastDensRef !== refTop || lastDensRange !== dispRange) {
        if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
        lastDensRef = refTop; lastDensRange = dispRange;
      }
      const len2 = spec.length * S.RTA_AMP_BINS;
      const floorN = percentileApprox(spec, 0.3);
      // Amplitude-graded weight: how far a point sits above the noise floor decides how
      // strongly it accumulates. Weak signals (>3 dB) still leave a light density cloud
      // so the density map covers the whole trace; the floor ripple itself stays out.
      const accW = (relDb: number): number => {
        if (relDb < 3) return 0;
        if (relDb >= 25) return 1;
        return 0.25 + 0.75 * ((relDb - 3) / 22);
      };
      const pushDensity = (nd: Float32Array, i: number, relDb: number, w: number) => {
        const b = Math.max(0, Math.min(S.RTA_AMP_BINS - 1, Math.round((refTop - spec[i]) / dB_PER_BIN)));
        const o = i * S.RTA_AMP_BINS;
        const bump = (bin: number, v: number) => {
          if (bin < 0 || bin >= S.RTA_AMP_BINS) return;
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
      if (!S.rtaDensity2d || S.rtaDensity2d.length !== len2) {
        const nd = new Float32Array(len2);
        for (let i = 0; i < spec.length; i++) {
          const w = accW(spec[i] - floorN);
          if (w <= 0) continue;
          pushDensity(nd, i, spec[i] - floorN, w);
        }
        S.setRtaDensity2d(nd);
      } else {
        const nd = S.rtaDensity2d;
        for (let i = 0; i < spec.length; i++) {
          for (let b = 0; b < S.RTA_AMP_BINS; b++) {
            const v = nd[i * S.RTA_AMP_BINS + b] * S.rtaFade;
            nd[i * S.RTA_AMP_BINS + b] = v > 0.05 ? v : 0;
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
      if (S.waterfallOn && S.rtaMode && !S.wfPaused) {
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
        renderAll();
      }
      return;
    }
    if (magic === 'FREQ') {
      if (event.data.byteLength !== 16 + points * 8) return;
      S.setFreqArray(new Float64Array(event.data, 16, points));
      S.setFreqVersion(version);
      const el = document.getElementById('info-pts');
      if (el) el.innerText = String(points);
      retrackMarkers();
    } else if (magic === 'POWR') {
      if (event.data.byteLength !== 16 + points * 4) return;
      if (version !== S.freqVersion) return;
      const raw = new Float32Array(event.data, 16, points);
      processTraces(raw);
      if (!S.rtaMode) evaluateSwpTrigger();  // software level trigger on consecutive sweeps
      noteFrameArrived();                    // armed: this trace IS the capture
      const now = performance.now();
      if (now - lastRender >= 33) {
        lastRender = now;
        renderAll();
      }
    }
  };
}

export function updateStatus(s: any) {
  if (!s || !s.req || !s.actual) return;
  if (s.caps) S.setFrequencyLimits(Number(s.caps.fmin), Number(s.caps.fmax));
  // STATUS top-level fields are the effective values for the active hardware mode.
  const isRtaStatus = s.mode === 'rta';
  if (s.req.rta?.center > 0) S.setRtaCenterHz(Number(s.req.rta.center));
  S.setCenterHz(Number(s.center));
  S.setSpanHz(Number(s.span));
  S.setRefLevel(Number(s.ref));
  S.setRefMode(s.ref_mode === 'auto' ? 'auto' : 'manual');
  S.setConfigVersion(Number(s.config_version) || 0);
  S.setCurrentRBW(Number(s.rbw));
  S.setCurrentVBW(Number(s.vbw));
  S.setRbwMode(s.rbw_mode);
  S.setVbwMode(s.vbw_mode);
  S.setCurrentPoints(Number(s.points) || Number(s.req.swp?.points) || 1001);
  S.setCurrentSpur(s.req.swp?.spur || s.spur || 'bypass');
  S.setSweepMs(s.sweep_ms || 0);
  S.setDeviceConnected(!!s.connected);
  syncGraphModeStatus(s.mode);
  syncFrequencyEditorStatus(s.response_to, S.configVersion);
  syncRefLevelStatus(s.response_to);
  syncAvgUI();
  syncSwpSpanStep(Number(s.req.swp?.span) || S.spanHz);

  const measKey = `${S.centerHz}|${S.spanHz}|${S.currentPoints}|${S.currentRBW}|${S.rbwMode}|${s.window}`;
  if (measKey !== S.lastMeasKey) {
    S.setLastMeasKey(measKey);
    const hadNormalization = S.traces.some(trace => trace.isNormalized && trace.reference);
    invalidateAllTraces();
    if (hadNormalization) showNormalizeClearedHint();
  }
  if (S.displayUnit !== 'dB' && s.mode !== 'sdr') S.setDisplayRef(S.refLevel);
  syncScaleButtons();

  const frequencyCommitted = s.response_to === 'SET_FREQ' || s.response_to === 'SET_RTA';
  updateFreqUIInputs(frequencyCommitted);
  const refText = Number.isInteger(S.refLevel) ? S.refLevel.toFixed(0) : S.refLevel.toFixed(1);
  setInput('input-ref', S.displayUnit === 'dB' ? '0' : refText);
  const refInput = document.getElementById('input-ref') as HTMLInputElement | null;
  const refSet = document.getElementById('btn-ref-set') as HTMLButtonElement | null;
  const refAuto = document.getElementById('btn-ref-auto') as HTMLButtonElement | null;
  if (refInput) refInput.disabled = S.refMode === 'auto';
  if (refSet) refSet.disabled = S.refMode === 'auto';
  const refDown = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  const refUp = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  const inAuto = S.refMode === 'auto';
  if (refDown) {
    refDown.disabled = inAuto || S.refLevel <= -50;
    refDown.title = inAuto ? t('auto') : t('ref_down');
  }
  if (refUp) {
    refUp.disabled = inAuto || S.refLevel >= 30;
    refUp.title = inAuto ? t('auto') : t('ref_up');
  }
  if (refAuto) {
    refAuto.classList.toggle('active', S.refMode === 'auto');
    refAuto.title = s.auto_ref_suspended ? t('auto_needs_atten') : '';
  }
  setInput('input-points', String(S.currentPoints));
  setSelect('select-rbw-mode', S.rbwMode);
  setSelect('select-vbw-mode', S.vbwMode);
  setSelect('select-spur', S.currentSpur);
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
        Math.abs(Number(option.value) - S.spanHz) < Math.abs(Number(best.value) - S.spanHz)
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
  }
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of && document.activeElement !== of && Math.abs(parseFloat(of.value) - S.displayOffset) > 0.01)
    of.value = S.displayOffset.toFixed(1);
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
export function updateFreqUIInputs(force = false) {
  const swpEditor = document.getElementById('swp-freq-settings');
  if (swpEditor?.dataset.dirty !== '1') {
    setInput('input-center', toUnit(S.centerHz, 'center').toFixed(4), force);
    setInput('input-span', toUnit(S.spanHz, 'span').toFixed(4), force);
    setInput('input-start', toUnit(S.centerHz - S.spanHz / 2, 'start').toFixed(4), force);
    setInput('input-stop', toUnit(S.centerHz + S.spanHz / 2, 'stop').toFixed(4), force);
  }
  setInput('input-rbw', toUnit(S.currentRBW, 'rbw').toFixed(2));
  setInput('input-vbw', toUnit(S.currentVBW, 'vbw').toFixed(2));
  const rtaEditor = document.getElementById('rta-freq-settings');
  if (S.rtaMode && rtaEditor?.dataset.dirty !== '1') {
    const u = S.units.rta_center || 'MHz';
    const scale = u === 'GHz' ? 1e9 : u === 'kHz' ? 1e3 : 1e6;
    setInput('input-rta-center', (S.rtaCenterHz / scale).toFixed(4), force);
  }
}

// i18n sync: connect button/status text
export function syncConnectBtn() {
  const bc = document.getElementById('btn-connect');
  if (bc) bc.textContent = S.deviceConnected ? t('status_connected') : t('connect');
}
