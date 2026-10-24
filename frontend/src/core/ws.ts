// WebSocket protocol layer + STATUS handling
import { requestRender } from '../render/redraw';
import { updateFreqUIInputs } from '../ui/freqInputs';
import * as S from './store';
import { decodeFrame } from './frames';
import { t, hasKey } from './i18n';
import { updateInfoBar } from '../render/infobar';
import {
  fillGnssDetail,
  syncGraphModeStatus,
  releaseGraphModePending,
  syncPresetStatus, rejectPresetRequest,
  rejectSdrAmSyncRequest,
  rejectSdrFilterRequest,
  disconnectSdrSession,
  syncScaleButtons,
  syncSwpSpanStep,
  syncSdrPanel,
  currentGraphMode,
} from '../ui/controls';
import { invalidateAllTraces } from '../dsp/traces';
import { syncAvgUI, showNormalizeClearedHint } from '../ui/traceOps';
import { accumulateTrace } from '../dsp/accumulator';
import { pushSpectrumRow } from '../render/waterfall';
import { sampleSwpWaterfall } from '../ui/panels/waterfall';
import { onCommandSent, setWS } from './wsSend';
import { premiumRefClockLabel, refreshRefClockHint } from './refclock';
import { retrackMarkers } from './markerCommon';
import { processTraces } from '../dsp/traces';
import { confirmTriggerMetadata, noteFrameArrived, onTriggerReset, prepareTriggerFrame, resetTriggerConnection } from '../ui/triggerEvents';
import { confirmTriggerFromStatus, rejectTriggerEdit, triggerMode } from '../ui/triggerState';
import { evaluateSwpTrigger } from '../ui/swpTrigger';
import { onHarmResult } from '../meas/harmonic';
import { onPnmResult } from '../meas/phaseNoise';
import { confirmMeasurementSession, endMeasurementSessions } from '../meas/session';
import { plausibleSpectrum } from '../dsp/stats';
import { advanceDensity } from '../dsp/rtaDensity';
import { alignToDisplayWindow, sameFrequencyGrid } from '../dsp/grid';
import { getDisplayRef, setDisplayRef, noteDisplayRefReport, rejectDisplayRefRequest } from '../ui/displayRef';
import { updateTrackingMarkers } from '../dsp/markerTracking';
import { confirmRefLevel, refLevel } from '../ui/refState';
import { maybeRequestSdrFit, noteSdrRefState, noteTraceObservation, syncAutoScaleStatus }
  from '../ui/refAutoScale';
import { centerHz, spanHz, swpCenterHz } from '../ui/freqState';
import {
  rbwMode, vbwMode, currentRBW, currentPoints, currentSpur,
  frameDetector, traceAlign, pointsStrategy,
} from '../ui/swpState';
import { rtaAmpBins, rtaFade, waterfallOn, wfPaused } from '../ui/waterfallState';
import { displayOffset, displayUnit } from '../ui/displayState';
import { resetViewport, setCapture } from '../ui/spectrumViewport';
import { confirmSdrChannelRegistry } from '../sdr/iqStream';
import { syncAmpStatus } from '../ui/ampState';
import { rejectManualRef, syncLevelAvailability, syncRefClockStatus, rejectRefClockRequest, resetRefClockIntent } from '../ui/panels/refAmp';
import { confirmFrequencyStatus, rejectFrequencyCommit, resetFrequencyEditors } from '../ui/panels/frequency';
import { confirmResolutionStatus, rejectResolutionCommit, resetResolutionEditors } from '../ui/panels/resolution';
import { syncSweepStatus, rejectSweepRequest, resetSweepEditor } from '../ui/panels/waterfall';
import { acceptRtaAxisFrame, rejectRtaAxisTransition, resetRtaAxisTransition } from '../ui/panels/rta';
import { sdrCenterHz, sdrDecimate } from '../ui/sdrState';
import { deviceMode } from '../ui/graphMode';

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
let sdrTransition: { center: number; decimate: number; version: number } | null = null;

// Only spectrum arrivals use this trailing throttle; UI redraws remain synchronous.
const SPECTRUM_INTERVAL = 1000 / 30;
let spectrumTimer: number | null = null;
let pendingSpectrumWork: (() => void) | null = null;
let lastSpectrumDraw = -Infinity;

function cancelSpectrumWork() {
  if (spectrumTimer !== null) window.clearTimeout(spectrumTimer);
  spectrumTimer = null;
  pendingSpectrumWork = null;
}

function drawSpectrumFrame() {
  lastSpectrumDraw = performance.now();
  requestRender();
}

function scheduleSpectrumWork(work: () => void, immediate = false) {
  const mode = currentGraphMode(), view = S.viewMode, owner = deviceMode.get();
  // Rejected settling packets still consume an RTA processing slot without a draw.
  const lastWork = mode === 'rta' ? Math.max(lastSpectrumDraw, lastRtaProcess) : lastSpectrumDraw;
  const guardedWork = () => {
    if (currentGraphMode() === mode && S.viewMode === view && deviceMode.get() === owner) work();
  };
  if (immediate || performance.now() - lastWork >= SPECTRUM_INTERVAL) {
    cancelSpectrumWork();
    guardedWork();
    return;
  }
  pendingSpectrumWork = guardedWork;
  if (spectrumTimer !== null) return;
  spectrumTimer = window.setTimeout(() => {
    const latest = pendingSpectrumWork;
    spectrumTimer = null;
    pendingSpectrumWork = null;
    latest?.();
  }, Math.ceil(SPECTRUM_INTERVAL - (performance.now() - lastWork)));
}

onTriggerReset(cancelSpectrumWork);
onCommandSent(command => {
  if (command.cmd === 'SET_MODE' || command.cmd === 'SET_PRESET'
      || command.cmd === 'SET_RTA' || command.cmd === 'SET_TRIGGER') cancelSpectrumWork();
});

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
// The canvas repaint for a link change is driven here, not by the frame loop: a disconnected
// device sends no frames, so the warning would otherwise never be drawn (same rule as -12).
let lastDisconnected = false;

export function connectWS() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  cancelSpectrumWork();
  lastSpectrumDraw = -Infinity;
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  // The display connection carries neither audio nor IQ: SDR playback has its own `?audio=1`
  // socket inside the audio worker and the DSP has its own `?iq=1` socket inside the IQ
  // worker, so a busy main thread cannot starve either of them - and the display socket never
  // pays for a stream it does not render.
  const params = new URLSearchParams();
  params.set('noaudio', '1');
  params.set('noiq', '1');
  ws = new WebSocket(`${protocol}//${location.host}/ws?${params.toString()}`);
  setWS(ws);   // Key: all commands (send) go through the unified wsSend exit, must be initialized
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    cancelSpectrumWork();
    lastSpectrumDraw = -Infinity;
    reconnectDelay = 1000;
    S.setRtaData(null);
    S.setRtaDensity2d(null);
    resetRtaAxisTransition();
    send({ cmd: 'STATUS' });
    // First load synchronizes the backend with the UI's initial standard sweep view.
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
    cancelSpectrumWork();
    endMeasurementSessions();
    S.setDeviceConnected(false);
    disconnectSdrSession();
    resetTriggerConnection();
    releaseGraphModePending();
    resetFrequencyEditors();
    resetResolutionEditors();
    resetSweepEditor();
    resetRefClockIntent();
    updateInfoBar();
    setWS(null);
    ws = null;
    scheduleReconnect();
  };
  ws.onmessage = (event: MessageEvent) => {
    if (typeof event.data === 'string') {
      try {
        const msg = JSON.parse(event.data as string);
        if (msg.cmd === 'STATUS') { confirmMeasurementSession(msg); updateStatus(msg); }
        else if (msg.cmd === 'HARM') onHarmResult(msg.list, msg.session_id);
        else if (msg.cmd === 'PNM') onPnmResult(msg);
        else if (msg.cmd === 'ERROR') {
          releaseGraphModePending();
          if (msg.response_to === 'SET_TRIGGER') rejectTriggerEdit();
          if (msg.response_to === 'SET_REF') rejectManualRef();
          rejectFrequencyCommit(msg.response_to, msg.request_id);
          rejectResolutionCommit(msg.response_to, msg.request_id);
          rejectRtaAxisTransition(msg.response_to, msg.request_id);
          if (msg.response_to === 'SET_SWEEP') rejectSweepRequest(msg.request_id);
          rejectRefClockRequest(msg.response_to, msg.request_id);
          rejectPresetRequest(msg.response_to, msg.request_id);
          rejectSdrAmSyncRequest(msg.response_to, msg.request_id);
          rejectSdrFilterRequest(msg.response_to, msg.request_id);
          S.setStatusError(localizedError(msg));
        }
      } catch (error) {
        console.error('Invalid WebSocket JSON message', error);
      }
      return;
    }
    if (!(event.data instanceof ArrayBuffer)) return;
    const frame = decodeFrame(event.data);
    // Raw IQ has its own connection and cannot feed the spectrum renderer.
    if (frame === null || frame.kind === 'baseband') return;
    const { points } = frame;
    if (frame.kind !== 'rta' && frame.sweepMs > 0 && frame.sweepMs !== S.sweepMs) {
      S.setSweepMs(frame.sweepMs);
      updateInfoBar();
    }
    if (points < 2) return;
    // SWP software capture holds the SWEPT display only: RTA and SDR arrive as RTAF frames
    // with their own grids, and a hold left over from the swept mode must not freeze them.
    if (S.swpHold && frame.kind !== 'rta') return;
    if (frame.kind === 'rta') {
      if (S.rtaData && frame.version < S.rtaData.ver) return;
      if (!acceptRtaAxisFrame(frame.version, frame.startHz, frame.stopHz)) return;
      const captureFrame = triggerMode() === 'rta' ? prepareTriggerFrame(frame.trigger) : 'continue';
      if (captureFrame === 'ignore') return;
      // RTA frame layout lives in core/frames.ts (magic + ver + pts + wfLen + maxD +
      // startHz, then freq(f8) + spec(f4) + wfRow(u2) + stopHz(f8)); the decoder has
      // already validated every length, so nothing here has to re-derive strides.
      const { version: ver, maxDensity, startHz, stopHz } = frame;
      const capFreq = frame.freq;
      const capSpec = frame.spec;
      const wfRow = frame.wfRow;
      const pts = frame.points;
      // Keep native Hz and levels; uncovered display edges are left empty by the renderer.
      const { freq, spec, shifted } = alignToDisplayWindow(capFreq, capSpec, startHz, stopHz);
      const previous = S.rtaData;
      const geometryChanged = !previous || previous.ver !== ver
        || Math.abs(startHz - previous.startHz) > 0.5
        || Math.abs(stopHz - previous.stopHz) > 0.5
        || !sameFrequencyGrid(previous.freq, freq);
      if (currentGraphMode() === 'sdr' && (sdrCenterHz.pending() || sdrDecimate.pending())) {
        const center = sdrCenterHz.get(), decimate = sdrDecimate.get();
        if (!sdrTransition || sdrTransition.center !== center || sdrTransition.decimate !== decimate) {
          sdrTransition = { center, decimate, version: previous?.ver ?? -1 };
        }
        if (ver <= sdrTransition.version) return;
      } else sdrTransition = null;
      const processAt = performance.now();
      // The first packet of a new capture must replace both layers immediately.
      if (currentGraphMode() !== 'rta' && !geometryChanged && captureFrame !== 'first' && !S.trigArmed && processAt - lastRtaProcess < 30) return;
      const immediate = geometryChanged || captureFrame === 'first' || S.trigArmed;
      const processRtaFrame = () => {
        // Axis or trigger admission can change while a normal packet waits for its deadline.
        if (currentGraphMode() === 'rta') {
          if (!acceptRtaAxisFrame(ver, startHz, stopHz) || (S.rtaData && ver < S.rtaData.ver)) return;
          if (!immediate && triggerMode() === 'rta' && prepareTriggerFrame(frame.trigger) === 'ignore') return;
        }
        lastRtaProcess = currentGraphMode() === 'rta' ? performance.now() : processAt;
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
        if (!plausible && !settleOver && !S.trigArmed) return;
        if (!plausible) S.setBadData(true);       // past it, show the data and say so
        // SDR: the swept reference level is meaningless here (often 0 dBm against a -100 dBm
        // noise floor), so entering the mode asks the backend for one fit; the client applies the
        // reported target to the display scale (see ui/refAutoScale.ts). Nothing runs unattended.
        if (currentGraphMode() === 'sdr') {
          maybeRequestSdrFit();
        }
        // The RTA frequency window (center/span) changed -> every accumulation (probability
        // density, per-trace displays, waterfall rows) lives on the OLD frequency axis and
        // must be reset, otherwise stale dots/traces linger at wrong frequencies.
        const axisChanged = geometryChanged;
        if (axisChanged || captureFrame === 'first') {
          S.setRtaDensity2d(null);
          for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
          for (let ti = 0; ti < S.rtaAvgN.length; ti++) { S.rtaAvgN[ti] = 0; S.rtaAvgSum[ti] = null; S.rtaDone[ti] = false; }
          S.resetWaterfall();
        }
        // The frame's display window is the capture the zoom feature navigates in (design
        // §4.2); a changed window drops the view inside setCapture.
        setCapture({ lo: startHz, hi: stopHz });
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
        // Persistence belongs to the selected, mode-processed trace, never the raw waterfall.
        // Store raw dBm: offset is a render transform and must also work with no new frames.
        const active = S.traces[S.activeTraceIdx];
        const bins = rtaAmpBins.get();
        const range = S.totalDivs * S.dbPerDiv;
        S.setRtaDensity2d(advanceDensity(S.rtaDensity2d, S.rtaDisplays[S.activeTraceIdx],
          bins, rtaFade.get(), getDisplayRef() - displayOffset.get(), range / bins, active.mode, freq));
        updateTrackingMarkers();
        if (waterfallOn.get() && S.rtaMode && !wfPaused.get()) {
          // bitmap rows are often all-zero; derive waterfall row from the live trace
          pushSpectrumRow(spec, freq, startHz, stopHz, 100);
        }
        const el = document.getElementById('info-pts');
        if (el) el.innerText = String(pts);
        const captureComplete = triggerMode() === 'rta' && noteFrameArrived(frame.trigger);
        if (currentGraphMode() === 'rta') {
          // Capture listeners synchronously paint the completed packet and its overlay.
          if (captureComplete) lastSpectrumDraw = performance.now();
          else drawSpectrumFrame();
        } else {
          // SDR retains its existing 30 ms processing and 16 ms arrival redraw gates.
          const now = performance.now();
          if (axisChanged || captureFrame === 'first' || now - lastRender >= 16) {
            lastRender = now;
            requestRender();
          }
        }
      };
      if (currentGraphMode() === 'rta') scheduleSpectrumWork(processRtaFrame, immediate);
      else processRtaFrame();
      return;
    }
    if (frame.kind === 'freq') {
      if (frame.version !== S.freqVersion || !sameFrequencyGrid(S.freqArray, frame.freq)) {
        cancelSpectrumWork();
        lastSpectrumDraw = -Infinity;
      }
      if (!sameFrequencyGrid(S.freqArray, frame.freq)) {
        invalidateAllTraces();
        S.setSwpPrev(null);
        S.resetWaterfall();
      }
      S.setFreqArray(frame.freq);
      S.setFreqVersion(frame.version);
      if (frame.freq.length > 1) {
        setCapture({ lo: frame.freq[0], hi: frame.freq[frame.freq.length - 1] });
      }
      const el = document.getElementById('info-pts');
      if (el) el.innerText = String(points);
      retrackMarkers();
    } else if (frame.kind === 'powr') {
      if (frame.version !== S.freqVersion) return;
      processTraces(frame.power);
      const canvas = document.getElementById('spectrum');
      if (canvas) canvas.dataset.swpFrames = String(Number(canvas.dataset.swpFrames ?? 0) + 1);
      sampleSwpWaterfall(frame.power, S.freqArray);
      if (triggerMode() === 'std') evaluateSwpTrigger(frame.power);
      if (S.swpHold) {
        // The hit listener already painted the frozen sweep and trigger overlay.
        cancelSpectrumWork();
        lastSpectrumDraw = performance.now();
      } else scheduleSpectrumWork(drawSpectrumFrame);
    }
  };
}

export function updateStatus(s: any) {
  if (!s || !s.req || !s.actual) return;
  if (s.connected === false || s.mode !== deviceMode.get()) cancelSpectrumWork();
  document.body.dataset.deviceMode = String(s.mode);
  syncPresetStatus(s.response_to, s.request_id);
  S.setDeviceConnected(s.connected === true);
  syncGraphModeStatus(s.mode);
  if (s.caps) {
    S.setFrequencyLimits(Number(s.caps.fmin), Number(s.caps.fmax));
  }
  // STATUS top-level fields are the effective values for the active hardware mode.
  const isRtaStatus = s.mode === 'rta';
  confirmFrequencyStatus(s);
  if (s.mode !== 'rta' && s.mode !== 'sdr') swpCenterHz.confirm(Number(s.center));
  // -12 = APIRETVAL_WARNING_IFOverflow: the IF saturates when Ref is set low (gain rises as
  // Ref falls) and the device then stops delivering frames, so the display looks frozen.
  // The vendor's remedy is to RAISE the reference level. Shown in the canvas warning stack
  // (top-right) and as a pulsing outline on the Ref control. Cleared by a good frame.
  {
    const over = Number(s.status_warning) === -12;
    const disconnected = s.connected === false;
    const refEl = document.getElementById('input-ref') as HTMLInputElement | null;
    if (refEl) {
      refEl.classList.toggle('ref-warn', over);
      refEl.title = over ? t('if_overflow_hint') : '';
    }
    const warnings: string[] = [];
    if (over) warnings.push('!' + t('if_overflow_short'), '!' + t('if_overflow_hint'));
    if (disconnected) {
      warnings.push('!' + t('device_disconnected_short'), '!' + t('device_disconnected_hint'));
    }
    S.setStatusWarnings(warnings);
  // Warning changes need a repaint even when overflow or disconnect stops incoming frames.
  if (over !== lastOverflowWarning || disconnected !== lastDisconnected) {
    lastOverflowWarning = over;
    lastDisconnected = disconnected;
    // A disconnect invalidates the "currently drawn frame" assumption: drop the zoom view
    // (design §4.3). The toggle stays as the user set it.
    if (disconnected) resetViewport();
    requestRender();
  }
  }
  confirmRefLevel(s.mode, s.ref,
    s.response_to === 'SET_REF' ? s.req?.[s.mode === 'std' ? 'swp' : s.mode]?.ref : undefined);
  S.setConfigVersion(Number(s.config_version) || 0);
  confirmResolutionStatus(s);
  currentPoints.confirm(Number(s.points) || Number(s.req.swp?.points) || 1001);
  currentSpur.confirm(s.req.swp?.spur || s.spur || 'bypass');
  frameDetector.confirm(s.req.swp?.frame_detector || s.frame_detector || 'device_default');
  traceAlign.confirm(s.req.swp?.trace_align || s.trace_align || 'start');
  pointsStrategy.confirm(s.req.swp?.points_strategy || s.points_strategy || 'speed');
  if (typeof s.sweep_ms === 'number') S.setSweepMs(s.sweep_ms);
  if (typeof s.sweep_ms_meas === 'number') S.setSweepMsMeas(s.sweep_ms_meas);
  confirmTriggerFromStatus(s.req.rta ?? {});
  if (triggerMode() === 'rta') confirmTriggerMetadata(s.req.rta?.trigger_actual ?? {});
  syncAvgUI();
  syncSwpSpanStep(Number(s.req.swp?.span) || spanHz.get());

  // Internal harmonic retunes do not change the display window or invalidate its trace.
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
  if (s.mode === 'sdr') {
    if (typeof s.sdr?.ref === 'number') noteDisplayRefReport(s.sdr.ref);
  } else rejectDisplayRefRequest();
  // SDR owns two refs (its IQS level and the client's display scale): this brings the display onto
  // the level the SDR itself stores, once per entry, and tells the entry fit whether to run at all.
  noteSdrRefState(s.sdr);
  if (displayUnit.get() !== 'dB') setDisplayRef('mode', refLevel.get());
  syncScaleButtons();

  const frequencyCommitted = s.response_to === 'SET_FREQ' || s.response_to === 'SET_RTA';
  updateFreqUIInputs(frequencyCommitted);
  const cur = refLevel.get();
  const refText = Number.isInteger(cur) ? cur.toFixed(0) : cur.toFixed(1);
  // The editor holds the absolute reference level in dBm for Set and STATUS updates.
  // Relative display mode changes the rendered axis, not this hardware reference.
  setInput('input-ref', refText);
  const refInput = document.getElementById('input-ref') as HTMLInputElement | null;
  // Nothing locks the reference any more: Auto Scale is a one-shot action, so the input, the
  // Set button and the step arrows stay usable and only their pending state is shown.
  if (refInput) refInput.classList.toggle('ref-pending', refLevel.pending());
  syncLevelAvailability(s.mode);
  // The step arrows are never greyed at a range end any more: the range end is not a state the
  // user has to be locked out of, and the value that results from a press is clamped to the
  // device's own range silently (see panels/refAmp.ts adjustRefLevel).
  const refDown = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  const refUp = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (refDown) refDown.title = t('ref_down');
  if (refUp) refUp.title = t('ref_up');
  // Auto Scale feedback: the button glows while a fit is in flight (the backend reports it via
  // auto_ref.adjusting, and the client keeps its own fallback timer), then reports the result.
  syncAutoScaleStatus(s);
  // A refusal notice describes the trace in front of the user: withdraw it the moment the trace
  // no longer matches (first frame arrived, or the peak moved away from the decision's basis).
  noteTraceObservation(s.auto_ref);
  setInput('input-points', String(currentPoints.get()));
  setSelect('select-rbw-mode', rbwMode.get());
  setSelect('select-vbw-mode', vbwMode.get());
  setSelect('select-spur', currentSpur.get());
  setSelect('select-detector', s.detector || 'auto');
  setSelect('select-framedet', frameDetector.get());
  setSelect('select-tracealign', traceAlign.get());
  setSelect('select-points-strategy', pointsStrategy.get());
  const wsel = document.getElementById('select-window') as HTMLSelectElement;
  if (wsel && document.activeElement !== wsel && s.window != null) wsel.value = String(s.window);
  syncSweepStatus(s);
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
    // Decoded strings come from the backend (version_info.py, the same source as the official
    // About panel). The code falls back to the raw numbers when an older backend omits them.
    // The device bar shows the model and the three versions that an operator reads first
    // (MFW/FFW/API). The other rows (UID/HW/BUS/PMU/AGU/Warn) stay in the Device popover.
    set('info-device', dd.model_name || dd.model);
    set('dev-mfw', dd.mfw_str || dd.mfw);
    set('dev-ffw', dd.ffw_str || dd.ffw);
    set('dev-api', 'v' + (dd.api_str || dd.api_ver));
    set('dev-d-model', dd.model_name || dd.model);
    set('dev-d-uid', dd.uid);
    set('dev-d-hw', dd.hw);
    set('dev-d-mfw', dd.mfw_str || dd.mfw);
    set('dev-d-ffw', dd.ffw_str || dd.ffw);
    set('dev-d-bus', dd.bus_speed + ' / ' + (dd.bus_str || ('v' + dd.bus_ver))
        + (dd.bus_bandwidth ? ' / ' + Math.round(dd.bus_bandwidth / 1e6) + ' MB/s' : ''));
    set('dev-d-api', 'v' + (dd.api_str || dd.api_ver));
    set('dev-d-pmu', dd.pmu_str || dd.pmu || '-');
    set('dev-d-agu', dd.agu_str || dd.agu || '-');
    set('dev-d-warn', dd.warnings);
  }
  S.setLastGnss(s.gnss);
  for (const id of ['gnss-d-docxo', 'gnss-d-docxo_mode']) {
    const row = document.getElementById(id)?.closest<HTMLElement>('.param-row');
    if (row) row.hidden = s.has_docxo !== true;
  }
  const g = document.getElementById('info-gnss');
  if (g) {
    // The top bar shows only the locked state (details such as satellite count live in the detail popover)
    if (s.gnss?.stale) { g.textContent = t('status_stale'); g.style.color = 'var(--muted)'; }
    else if (s.gnss && s.gnss.lock) { g.textContent = t('status_locked'); g.style.color = 'var(--dot-on)'; }
    else { g.textContent = t('status_nolock'); g.style.color = 'var(--dot-off)'; }
    // Update the detail popover synchronously (if open)
    const pop = document.getElementById('gnss-popover');
    if (pop && pop.style.display !== 'none') fillGnssDetail();
  }
  const rc = document.getElementById('select-refclk') as HTMLSelectElement;
  if (rc) {
    let premium = rc.querySelector<HTMLOptionElement>('option[value="premium"]');
    const available = s.has_premium_ref_clock === true;
    if (available && !premium) {
      premium = document.createElement('option');
      premium.value = 'premium';
      rc.appendChild(premium);
    } else if (!available && premium) {
      premium.remove();
      premium = null;
    }
    if (premium) premium.textContent = premiumRefClockLabel(s);
  }
  refreshRefClockHint(s, s.response_to);
  syncRefClockStatus(s);
  if (s.amp) syncAmpStatus(s.amp);
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
  confirmSdrChannelRegistry(Array.isArray(s.actual.channel_registry) ? s.actual.channel_registry : null);
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
