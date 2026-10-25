// Control commands + data-action binding + panel collapse + marker ops + canvas interaction
import * as S from '../core/store';
import { send } from '../core/wsSend';

import { clearSdrEntryState, postRefNotice, requestSdrEntryFit, resetAutoScaleState } from './refAutoScale';
import { sdrAgc, sdrAmSync, sdrAudioOn, sdrCenterHz, sdrDecimate, sdrDeemph, sdrDemod, sdrIfbw, sdrListenHz, sdrNr, sdrNrAlgo, sdrNrAtten, sdrNrStrength, sdrPitchHz, sdrSpanHz, sdrSquelch, sdrSstvMode, sdrVolume, SDR_STEP_QUICK_HZ, currentSdrStepHz, estimatedCaptureSpanHz, hasStoredSdrPrefs, renderSdrState, resetSdrState, setSdrStepForCurrentMode, sdrLocateMode, sdrChannelStepHz, sdrChannelAnchorHz, type NrAlgo } from './sdrState';
import { bindSdrTuningUi, renderSdrTuningUi, cancelSdrTuningEdit } from './sdrTuningUi';
import { bindSdrFilter, cancelSdrFilterEdit, filterSpec, filterValueForMode, rememberConfirmedFilter, renderSdrFilter } from './sdrFilter';
import { sdrFilterLimits } from './sdrState';
import { clearPassbandView } from './spectrumViewport';
import { cancelFilterDrag, endFilterDrag, filterDragging, filterCenterAt, filterEdgeAt, moveFilterDrag, setFilterHover, startFilterDrag, viewSdrPassband } from './sdrFilterGesture';
import { cancelSdrTuneRequest, confirmSdrTuneReport, requestSdrTune, resetSdrTuneSession, stopSdrTuneRepeat } from './sdrTuneRequest';
import { beginSdrLocateClick, cancelSdrLocate, commitSdrLocateClick, updateSdrLocateHover } from './sdrLocate';
import { resetCarrierEvidence } from '../dsp/carrierSnap';
import { parseFreqUnit } from '../core/units';
import { centerHz, swpCenterHz } from './freqState';
import { updateInfoBar } from '../render/infobar';
import { requestRender } from '../render/redraw';
import { getDisplayPowers } from '../dsp/peaks';

import { normRefWinUser } from './measurePrefs';
import { switchTraceTab, toggleFreeze, setTraceMode, clearRtaTrace, setTraceAverage, exportActiveTraceCsv, exportPeakListCsv, syncFreezeBtn, syncAvgUI, rememberTraceMode, applyTraceFamilyMode, resetTraceFamilyModes, SDR_DEFAULT_AVG } from './traceOps';
import { exportSpectrumPng } from './exportImage';
import { normalizeActiveTrace, resetActiveTraceNormalize, updateNormalizeStatusUI, setNormalizeReferenceWindow } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
import { togglePeakList, peakThrManual, peakThrAuto, resetPeakThr, initPeakCountUi } from '../render/peaklist';
import { measToggle, measTab, applyMeasUI, setMeasButtons } from './measure';
import { measureAmp, clearAmp } from '../meas/amplitude';
import { measureChannel, clearChannel } from '../meas/channel';
import { measHarmApply, autoHarmSpan } from '../meas/harmonic';
import { measPnmApply, repaintPnm } from '../meas/phaseNoise';

import { t } from '../core/i18n';
import { getUiScale } from '../core/uiScale';
import { resetTrigger, syncTriggerMode } from './triggerState';
import { axisBandAt, axisDragging, axisDragEnd, axisDragMove, axisDragStart, axisWheel, confirmSdrAxisWindow, rejectAxisRef, resetAxisPreview, type Axis } from './axisDrag';
import { isSpectrumZoomOn, syncSpectrumZoomUi } from './spectrumZoomUi';
import {
	cancelMarqueePreview,
	commitMarquee,
	isMarqueeDrag,
	resetViewForDeviceGesture,
	spectrumFreqAtX,
	spectrumWheelZoom,
	updateMarqueePreview,
} from './spectrumZoomGestures';
import { minSpanHz, zoomEnabled } from './spectrumViewport';
import { resetViewport } from './spectrumViewport';
import { openRefClockDetail, closeRefClockDetail } from '../core/refclock';
import { prepareSdrAudioTransition, setSdrAudioEnabled, setSdrAudioMode } from '../audio/sdrAudio';
import { initSdrDemodGroup } from './sdrDemodGroup';
import { ft8DialFor } from '../sdr/ft8Log';
import { canToggleAmSync, confirmAmSyncGeneration, decodeChannel, initDecodeWindow, refreshDecodeWindow, setAmInfo, setDecodeActive, setDecodeChannelMode, toggleDecodeWindow } from './decodeWindow';
import { resetSdrIq, setSdrIqEnabled, setSstvModePin } from '../sdr/iqStream';
import { sdrModeIds } from '../sdr/registry';
import { resetLimits } from './limits';

// Panel modules (report finding P1-5). controls.ts keeps the wiring (event binding, canvas
// interaction, mode/preset orchestration) and imports the actions it dispatches; the
// re-exports below keep the previous public surface for the rest of the app.
import { applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetFrequencyEditors, resetSpanStepAuto, stepSwpSpan, syncSwpSpanStep, updateCustomSpanStep } from './panels/frequency';
import { closeDeviceDetail, fillDeviceDetail } from './panels/deviceInfo';
import { applyPoints, applyRBW, applyVBW, markResolutionDirty, resetResolutionEditors, syncResolutionAvailability, setFrameDetector, setPointsStrategy, setSpurMode, setTraceAlign, setWindow } from './panels/resolution';
import { applyRta, clearRtaAccum, resetRtaDensityCfg, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins, setRtaDensContrast, setRtaDensWidth, setRtaDensLayers, setRtaDensSource, setRtaHalflife, toggleDensitySettings } from './panels/rta';
import { resetAmpIntents } from './ampState';
import { activeMarkerNextPeakLeft, activeMarkerNextPeakRight, activeMarkerNextValleyLeft, activeMarkerNextValleyRight, activeMarkerPeak, activeMarkerValley, markerToCenter, placeMarkerFromX, selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll, updateMarkersAllBtn } from './panels/markers';
import { adjustRefLevel, commitLevelOffset, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel, setScale,
  syncRefUI, syncLevelAvailability, resetRefClockIntent, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
import { resetWf, setSweepSpeed, syncSweepInput, bindSweepEditor, resetSweepEditor, setWfPaused, toggleWaterfall, toggleWfPause, initWfSplit, resetWfSplit } from './panels/waterfall';
import { closeGnssDetail, fillGnssDetail } from './panels/gnss';
import { toggleAllGroups, toggleGroup } from './panels/groups';
import { commitUnitField } from './panels/commit';
import { resetAll } from '../core/params';

// ── Frequency linking ──

// Mode-driven control policy from the backend's own command table (/api/schema). The hand
// lists in syncGraphModeStatus cover the obvious panels; this adds the schema truth for
// controls whose command is refused in the active mode — CSS dimming alone let the click
// through to a rejection dialog in RTA (FrameDet / TraceAlign / PointsStrategy / Spur).
type CommandPolicy = { session_exclusive: boolean; swp_only: boolean; denied_in_sdr: boolean };
let commandPolicy: Record<string, CommandPolicy> | null = null;
const CONTROL_COMMANDS: Readonly<Record<string, string>> = {
  'select-framedet': 'SET_FRAMEDET',
  'select-tracealign': 'SET_TRACEGRID',
  'select-points-strategy': 'SET_TRACEGRID',
  'select-spur': 'SET_SPUR',
};

function loadCommandPolicy(): void {
  fetch('/api/schema')
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`status ${r.status}`))))
    .then((schema: { commands?: Record<string, CommandPolicy> }) => {
      commandPolicy = schema?.commands ?? null;
      applyCommandPolicy(currentGraphMode());
    })
    .catch(() => {
      // No schema (offline / older backend): the hand lists in syncGraphModeStatus stay
      // the only gate, exactly as before.
    });
}

/** Disable the mapped controls whose command the active mode refuses. */
export function applyCommandPolicy(mode: string): void {
  if (!commandPolicy) return;
  const isRtaLike = mode === 'rta' || mode === 'sdr';
  const isSdr = mode === 'sdr';
  const owns = measurementOwnsDevice();
  for (const [id, cmd] of Object.entries(CONTROL_COMMANDS)) {
    const flags = commandPolicy[cmd];
    if (!flags) continue;
    const off = (flags.swp_only && isRtaLike)
      || (flags.denied_in_sdr && isSdr)
      || (flags.session_exclusive && owns);
    const el = document.getElementById(id);
    if (el instanceof HTMLSelectElement || el instanceof HTMLInputElement
        || el instanceof HTMLButtonElement) el.disabled = off;
  }
}

export function connectDevice() { send({ cmd: 'CONNECT' }); }

// ── Marker operations ──
// Graph-mode + display-reference requests are owned by ui/graphMode.ts and ui/displayRef.ts.
// They apply the four disciplines (id-matched ack, supersede, timeout notice, visible
// divergence); this module only translates them to the DOM.
import { currentGraphMode, deviceMode, graphMode, graphModeDiverges, isGraphMode, measurementOwnsDevice, pendingGraphMode, requestGraphMode, confirmGraphMode, resetGraphMode, setGraphModeTimeoutHandler } from './graphMode';
import { resetDisplayRefMode, setDisplayRef, setDisplayRefTimeoutHandler } from './displayRef';
import { refLevel } from './refState';

let initialSdrNr: { on: boolean; strength: number; algorithm: NrAlgo; attenuation: number } | null = {
  on: sdrNr.get(), strength: sdrNrStrength.get(), algorithm: sdrNrAlgo.get(), attenuation: sdrNrAtten.get(),
};
let initialSstvMode: number | null = (() => {
  try { return localStorage.getItem('web-sa-sdr-sstv-mode') !== null ? sdrSstvMode.get() : null; }
  catch { return null; }
})();
let initialSdrAmSync: boolean | null = (() => {
  try { return localStorage.getItem('web-sa-sdr-am-sync') !== null ? sdrAmSync.get() : null; }
  catch { return null; }
})();
let amSyncSerial = 0;
let pendingAmSync: { requestId: string; timer: number } | null = null;

function clearAmSyncRequest(): void {
  if (pendingAmSync) window.clearTimeout(pendingAmSync.timer);
  pendingAmSync = null;
}

function requestSdrAmSync(enabled: boolean): void {
  clearAmSyncRequest();
  setAmInfo(null, 1);
  sdrAmSync.set(enabled);
  const epoch = sdrAmSync.epoch;
  const requestId = `sdr-am-sync-${++amSyncSerial}`;
  pendingAmSync = { requestId, timer: window.setTimeout(() => {
    pendingAmSync = null;
    if (sdrAmSync.epoch !== epoch || sdrAmSync.desiredValue() === null) return;
    sdrAmSync.reset();
    refreshDecodeWindow();
  }, 3000) };
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, am_sync: enabled, request_id: requestId });
  refreshDecodeWindow();
}

/** A rejection must belong to this detector request, not another primary-channel edit. */
export function rejectSdrAmSyncRequest(responseTo?: string, requestId?: string | number): void {
  if (responseTo !== 'SET_SDR_CHANNEL' || !pendingAmSync || pendingAmSync.requestId !== requestId) return;
  clearAmSyncRequest();
  sdrAmSync.reset();
  refreshDecodeWindow();
}

/** A lost control/device connection ends the receiver session and all unconfirmed detector intent. */
export function disconnectSdrSession(): void {
  resetSdrTuneSession();
  resetCarrierEvidence();
  cancelSdrTuningEdit();
  clearAmSyncRequest();
  sdrAmSync.reset();
  resetSdrAgcRequest();
  clearReceiverEdit();
  sdrDemod.reset();
  sdrIfbw.reset();
  clearPassbandView();
  sdrFilterLimits.confirm(null);
  cancelSdrCanvasGesture();
  cancelSdrFilterEdit();
  confirmAmSyncGeneration(null);
  setSdrIqEnabled(false);
  resetSdrIq();
  setDecodeActive(false);
}

function amSyncSessionAvailable(): boolean {
  return S.deviceConnected && deviceMode.get() === 'sdr' && !measurementOwnsDevice()
    && !(S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm'));
}

let sdrAudioHandoffTimer: number | null = null;

// Transient hint notices. A held notice (a timeout report) must not be overwritten by the
// high-frequency pending-state refresh, so it owns the field until it expires.
const hintHoldUntil = new Map<string, number>();
function setHint(id: string, text: string, holdMs = 0): void {
  const el = document.getElementById(id);
  if (!el) return;
  const now = Date.now();
  if (holdMs === 0 && (hintHoldUntil.get(id) ?? 0) > now) return;   // a held notice owns it
  if (holdMs > 0) hintHoldUntil.set(id, now + holdMs);
  else hintHoldUntil.delete(id);
  el.textContent = text;
}
function flashHint(id: string, text: string, holdMs = 5000): void {
  setHint(id, text, holdMs);
  window.setTimeout(() => {
    if ((hintHoldUntil.get(id) ?? 0) <= Date.now()) setHint(id, '');
  }, holdMs + 50);
}

setGraphModeTimeoutHandler((want) => {
  flashHint('mode-hint', t('mode_switch_timeout', { mode: want.toUpperCase() }));
  syncModeButtons();
});
setDisplayRefTimeoutHandler(() => {
  // Same place as the Auto Scale outcome: it is a message about the reference the user is
  // looking at, and the input already carries the local "unconfirmed" outline (ref-pending).
  postRefNotice(t('ref_switch_timeout'), 5000);
  refLevel.reset();
  rejectAxisRef();
  syncRefUI(true);
  requestRender();
});

function deferSdrAudioPreference() {
  if (sdrAudioHandoffTimer !== null) window.clearTimeout(sdrAudioHandoffTimer);
  // Drop samples from the pre-SDR/default demod chain until the final SDR commands settle.
  setSdrAudioEnabled(false);
  sdrAudioHandoffTimer = window.setTimeout(() => {
    sdrAudioHandoffTimer = null;
    if (currentGraphMode() === 'sdr') applySdrAudioPreference();
  }, 800);
}

/** Mark the mode buttons while a request is unconfirmed. They stay enabled so that a new
 *  click supersedes the request instead of being swallowed (discipline 2 + 4). */
function syncModeButtons(): void {
  const want = pendingGraphMode();
  const busy = want !== null && graphModeDiverges();
  for (const id of ['btn-mode-rta', 'btn-mode-sdr']) {
    const b = document.getElementById(id) as HTMLButtonElement | null;
    if (!b) continue;
    b.classList.toggle('pending', busy);
    b.disabled = false;
  }
  setHint('mode-hint', busy ? t('mode_switching', { mode: want!.toUpperCase() }) : '');
}

export function setGraphMode(mode: string) {
  const target: 'std' | 'rta' | 'sdr' =
    mode === 'rta' ? 'rta' : mode === 'sdr' ? 'sdr' : 'std';
  if (pendingGraphMode() === null && target === currentGraphMode()) return;
  if (target !== 'std' && S.measOn) {
    exitMeasModePub(false);
    S.setMeasOn(false);
    const button = document.getElementById('btn-meas-onoff');
    if (button) button.textContent = t('off');
    setMeasButtons(false);
  }
  // A newer request replaces the previous one and restarts its timeout (discipline 2).
  requestGraphMode(target);
  cancelSdrTuneRequest();
  cancelSdrCanvasGesture();
  cancelSdrFilterEdit();
  if (target !== 'sdr') {
    cancelSdrTuningEdit();
    clearReceiverEdit();
    sdrDemod.reset();
    sdrIfbw.reset();
  }
  syncModeButtons();
  syncResolutionAvailability();
  // Sweep-to-SDR handoff: entering SDR from the swept view demodulates the frequency
  // the user located (active marker), or the current centre if no marker is set.
  if (target === 'sdr') {
    // Priority: an explicit hand-off (Shift+click / peak row already called sdrCenterHz.set), then
    // the user's own SDR tuning, then - first run only - the active marker or the swept centre.
    // Returning to SDR used to re-derive everything from the swept view, which discarded the
    // tuning and the listening setup the user had left there (reported).
    if (!sdrCenterHz.pending()) {
      const remembered = sdrCenterHz.confirmedValue();
      if (remembered !== null && remembered > 0) {
        sdrCenterHz.set(remembered);           // re-assert: the entry below sends it
      } else {
        const m = S.markers.find(x => x.enabled && x.freq != null);
        // Prefer the last centre confirmed by a SWP-family STATUS: centerHz.get() is refreshed
        // from every STATUS (including SDR ones), so it can still hold the value from before
        // a preset/re-tune when this runs.
        const base = swpCenterHz.get() > 0 ? swpCenterHz.get() : centerHz.get();
        sdrCenterHz.set((m && m.freq) ? m.freq : base);
      }
    }
    deferSdrAudioPreference();
  } else {
    sdrCenterHz.reset();
    if (sdrAudioHandoffTimer !== null) {
      window.clearTimeout(sdrAudioHandoffTimer);
      sdrAudioHandoffTimer = null;
    }
    setSdrAudioEnabled(false);
    setSdrIqEnabled(false);
  }
  send({ cmd: 'SET_MODE', mode: target });
}

/** Drop a pending request (socket loss / command error): the UI follows the backend again. */
export function releaseGraphModePending() {
  resetGraphMode();
  syncModeButtons();
  syncResolutionAvailability();
}

export function syncGraphModeStatus(mode: string) {
  // The raw reported mode is recorded FIRST: the measurement sessions are not view modes, but they
  // own the device, and everything mode-gated has to see that (see `deviceMode`).
  deviceMode.confirm(mode);
  syncTriggerMode(mode);
  syncLevelAvailability(mode);
  syncResolutionAvailability(mode);
  setDecodeActive(S.deviceConnected && mode === 'sdr');
  if (!isGraphMode(mode)) {
    cancelSdrCanvasGesture();
    cancelSdrTuningEdit();
    applyMeasUI();
    return;
  }
  // The family being left, read before the confirm: that is the trace mode to remember.
  const previousMode = currentGraphMode();
  // The very first STATUS after a page load "confirms" std without any mode change having
  // happened — and the SWP FREQ frame that seeded the zoom capture will not repeat until
  // the next reconfiguration. Only a real confirmed-mode SWITCH may drop the view (§4.3).
  const hadConfirmedMode = graphMode.confirmedValue() !== null;
  // Discipline 1: an older reply (a STATUS still on another mode) is not our answer.
  const changed = confirmGraphMode(mode);
  syncModeButtons();
  if (!changed) { applyMeasUI(); return; }
  resetDisplayRefMode();
  resetTrigger();
  const isRtaLike = mode === 'rta' || mode === 'sdr';
  const isSdr = mode === 'sdr';
  S.setRtaMode(isRtaLike);
  S.setViewMode(isRtaLike ? 'rta' : 'std');
  S.setSdrMode(isSdr);
  applyMeasUI();
  // An axis gesture previews the window of the mode it started in; a mode change invalidates it.
  resetAxisPreview();
  cancelSdrCanvasGesture();
  if (!isSdr) cancelSdrTuningEdit();
  // A display view belongs to the data of the mode it was drawn in (design §4.3): drop it
  // on any confirmed mode change; the toggle itself stays as the user set it.
  if (hadConfirmedMode) {
    resetViewport();
    cancelZoomGesture();
    syncSpectrumZoomUi();
  }
  // Each display family keeps its own trace mode: SDR opens on an Average (the depth defaults to
  // the panel's 16), the swept/RTA views keep Clear Write, and whatever the user picked inside a
  // family is what that family shows when it is entered again. Applied here - once, on a CONFIRMED
  // mode change - and never from the per-frame STATUS loop, which would overwrite a gesture the
  // user just made (see ui/traceOps.ts).
  rememberTraceMode(previousMode === 'sdr');
  applyTraceFamilyMode(isSdr);
  if (isSdr && S.deviceConnected) {
    // Decode events and receiver telemetry arrive independently of speaker playback.
    setSdrIqEnabled(true);
    deferSdrAudioPreference();
    if (initialSdrNr) {
      sdrNr.set(initialSdrNr.on);
      sdrNrStrength.set(initialSdrNr.strength);
      sdrNrAlgo.set(initialSdrNr.algorithm);
      sdrNrAtten.set(initialSdrNr.attenuation);
      initialSdrNr = null;
    }
    sendSdrNr();
    if (initialSstvMode !== null) {
      sdrSstvMode.set(initialSstvMode);
      setSstvModePin(initialSstvMode < 0 ? null : initialSstvMode, 1);
      initialSstvMode = null;
      refreshDecodeWindow();
    }
    // Auxiliary receivers have no browser controls; never restore hidden saved receivers.
    send({ cmd: 'SET_SDR_CHANNEL', channel: 2, enabled: false });
    send({ cmd: 'SET_SDR_CHANNEL', channel: 3, enabled: false });
  } else {
    // Leaving SDR stops the audio PIPELINE; the user's preference is theirs and must survive the
    // trip. Writing it off here is what made "audio on" silently become "audio off" after a visit
    // to RTA/SWP (measured: localStorage flipped 1 -> 0 on the way out).
    setSdrAudioEnabled(false);
    setSdrIqEnabled(false);
    syncSdrAudioButton();
    clearSdrEntryState();       // the entry hand-off (display scale + one fit) starts over next time
  }
  const modeButton = document.getElementById('btn-mode-rta');
  if (modeButton) modeButton.classList.toggle('active', mode === 'rta');
  const sdrButton = document.getElementById('btn-mode-sdr');
  if (sdrButton) sdrButton.classList.toggle('active', isSdr);
  localStorage.setItem('web-sa-mode', mode);
  if (isRtaLike) restoreRtaDensityCfg();

  document.body.classList.toggle('rta-mode', isRtaLike);
  const rtaDisable = [
    'select-smooth', 'select-refwin', 'btn-normalize', 'select-window',
    'select-detector', 'select-framedet', 'select-tracealign', 'select-points-strategy',
    'select-spur', 'input-points', 'btn-points',
  ];
  // SDR-only: controls that do not apply to the IQ receive path (SWP RBW/VBW/sweep/spur
  // and the device trigger). Gain/amp, reference clock and the shared Ref stay enabled.
  const setDisabled = (id: string, off: boolean) => {
    const el = document.getElementById(id) as HTMLElement | null;
    if (!el) return;
    if (el instanceof HTMLInputElement || el instanceof HTMLSelectElement
        || el instanceof HTMLButtonElement) {
      el.disabled = off;
    }
    el.querySelectorAll('input,select,button').forEach((c) => {
      (c as HTMLInputElement).disabled = off;
    });
  };
  ['select-rbw-mode', 'input-rbw', 'unit-rbw-group',
    'select-vbw-mode', 'input-vbw', 'unit-vbw-group', 'btn-vbw-set',
    'select-sweep-mode', 'sweep-panel',
    'select-spur', 'btn-gapfill',
  ].forEach((id) => setDisabled(id, isSdr));
  rtaDisable.forEach((id) => {
    const element = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
    if (element) element.disabled = isRtaLike;
  });
  const rbwSelect = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
  if (rbwSelect) {
    for (const option of rbwSelect.options) {
      option.disabled = mode !== 'std' && option.value !== 'auto' && option.value !== 'manual';
    }
  }
  // The schema's own verdict for the mapped controls, applied after the hand lists: in RTA
  // this is what actually disables FrameDet / TraceAlign / PointsStrategy / Spur.
  applyCommandPolicy(mode);
  syncResolutionAvailability(mode);
  const resetButton = document.querySelector(
    '[data-action="reset-norm"]') as HTMLButtonElement | null;
  if (resetButton) resetButton.disabled = isRtaLike;
  // The density map (Persist/Grain) is drawn from the non-real-time frames only: RTA and SDR own it,
  // and in the swept mode there is nothing to accumulate, so the row is disabled there instead of
  // sitting there doing nothing (reported: "does trace persist not work in SWP?").
  for (const id of ['select-rta-fade', 'select-rta-bins', 'select-rta-dens-source',
    'select-rta-dens-layers', 'select-rta-dens-contrast', 'select-rta-dens-width']) {
    setDisabled(id, mode === 'std');
  }
  const rtaFrequency = document.getElementById('rta-freq-settings');
  const swpFrequency = document.getElementById('swp-freq-settings');
  const sdrSettings = document.getElementById('sdr-settings');
  if (rtaFrequency) rtaFrequency.style.display = mode === 'rta' ? '' : 'none';
  if (swpFrequency) swpFrequency.style.display = mode === 'std' ? '' : 'none';
  if (sdrSettings) sdrSettings.style.display = isSdr ? '' : 'none';
  if (isRtaLike) S.resetWaterfall();
  if (isSdr && sdrCenterHz.pending() && sdrCenterHz.get() > 0) {
    const f = sdrCenterHz.get();
    if (!hasStoredSdrPrefs()) {
      // First run (or right after a Preset): pick a sensible listening setup for the band the
      // user landed in. Once they have their own preferences, those win - the band presets
      // (FM/AIR/VHF/UHF) are the deliberate way to switch bands.
      const inFm = f >= 87.5e6 && f <= 108e6;
      const inAir = f >= 118e6 && f <= 137e6;
      sdrDecimate.set(16);
      sdrDemod.set(inFm ? 'wfm' : 'am');
      sdrIfbw.set(inFm ? 180000 : (inAir ? 25000 : 12000));
      sdrDeemph.set(-1);
    }
    const decimate = sdrDecimate.get() || 16;
    sdrSpanHz.set(estimatedCaptureSpanHz(decimate));  // estimate until the device reports it
    sdrListenHz.set(sdrListenHz.get() > 0 ? sdrListenHz.get() : f);
    renderSdrState();
    send({ cmd: 'SET_SDR', center: f, decimate });
    send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: sdrListenHz.get() });
    applySdrDemod();
  }
  // Entry setup sends SET commands, which cancel older fits. Arm the new one only after
  // that hand-off finishes; an existing SDR hardware level is preserved by the fit owner.
  if (isSdr) requestSdrEntryFit();
  requestRender();
}

// Density persistence/grain restored on page load + RTA entry (independent memory keys)
// ── SDR mode controls ──

function sdrNumber(id: string, fallback: number): number {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  const v = el ? parseFloat(el.value) : NaN;
  return isFinite(v) ? v : fallback;
}

export function applySdr() {
  cancelSdrTuneRequest();
  const centerMhz = sdrNumber('input-sdr-center', 1000);
  const decimate = Math.round(sdrNumber('select-sdr-decimate', 32));
  const center = centerMhz * 1e6;
  // Record what the user typed as the intent; the STATUS confirms it later.
  sdrCenterHz.set(center);
  sdrDecimate.set(decimate);
  sdrListenHz.set(center);
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR', center, decimate });
  // Setting the wideband centre also tunes the demodulator there.
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: center });
  // Apply the same band -> demod rule as the SWP/RTA handoff, otherwise entering SDR
  // directly and typing a broadcast frequency keeps the previous demod (e.g. AM on an FM
  // station = noise).
  const inFm = center >= 87.5e6 && center <= 108e6;
  const inAir = center >= 118e6 && center <= 137e6;
  if (inFm || inAir) {
    sdrDemod.set(inFm ? 'wfm' : 'am');
    sdrIfbw.set(inFm ? 180000 : 25000);
    renderSdrState();
    applySdrDemod();
  }
  const l = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (l) l.value = centerMhz.toFixed(6);
}

// Changing only the capture bandwidth keeps the listen frequency unchanged. The tuning step
// follows the bandwidth on its own: renderSdrState shows the stored step for it, or the
// span-derived default, and the slots keep no second copy that could disagree.
export function applySdrBw() {
  cancelSdrTuneRequest();
  const decimate = Math.round(sdrNumber('select-sdr-decimate', 32));
  sdrDecimate.set(decimate);
  const center = sdrCenterHz.get();
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR', center, decimate });
}

export function applySdrTune() {
  const listenMhz = sdrNumber('input-sdr-listen', 1000);
  const f = listenMhz * 1e6;
  requestSdrTune(f);
}

/**
 * Apply the step the user typed, read in the unit shown by the shared unit buttons. The
 * value is stored for the current capture bandwidth, so each bandwidth keeps the step its
 * user chose for it. An empty or invalid box leaves the current step alone.
 */
export function applySdrStep() {
  const hz = parseFreqUnit('sdr_step');
  if (!(hz > 0)) return;
  setSdrStepForCurrentMode(hz);
  renderSdrTuningUi();
}

/**
 * Move the wideband centre by a part of the capture span. This is the SDR edge push: a drag that
 * touches either side of the plot. One writer for that move keeps the wire payload equal to the
 * slot values. The edge push put the slots themselves on the wire. JSON made objects of them, and
 * the backend answered "center must be a number" for each push. The operator then had to dismiss
 * a stack of dialogs after a drag to a side.
 */
export function sdrPanCentreBy(fraction: number) {
  const span = sdrSpanHz.get();
  const center = sdrCenterHz.get() + span * fraction;
  // An unknown span cannot move the centre, and 0/NaN must never reach the device as a centre.
  if (!(span > 0) || !(center > 0)) return;
  sdrCenterHz.set(center);
  send({ cmd: 'SET_SDR', center, decimate: Math.round(sdrDecimate.get()) });
}

/**
 * To Center: make the listen frequency the wideband centre. The demodulator is already on
 * the signal, so only the capture window moves around it. SET_SDR retunes the demod to the
 * new centre, so the follow-up SET_SDR_TUNE puts the listen frequency back where it was.
 */
export function sdrToCenter() {
  const listen = sdrListenHz.get();
  if (!(listen > 0)) return;
  cancelSdrTuneRequest();
  sdrCenterHz.set(listen);
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR', center: listen, decimate: Math.round(sdrDecimate.get()) });
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: listen });
}

export function applySdrDemod(restoringMode = false, filterOnly = false) {
  const mode = sdrDemod.get();
  setSdrAudioMode(1, mode);
  if (!filterSpec(mode).editable && sdrIfbw.get() !== filterSpec(mode).defaultHz) {
    sdrIfbw.set(filterSpec(mode).defaultHz);
  }
  const ifbw = sdrIfbw.get();
  const deemph = sdrDeemph.get();
  const volume = sdrVolume.get();
  const squelch = sdrSquelch.get();
  // Only a demod-mode / IF-bandwidth / de-emphasis change rebuilds the chain and needs the
  // reset handshake. Volume/squelch/AGC are applied live, so muting them would just add a
  // gap. "Changed" is the slot's own pending state now, not a separate copy of the last
  // value sent (those caches were one more thing that could disagree with the truth).
  if (sdrDemod.pending() || sdrIfbw.pending() || sdrDeemph.pending()) {
    prepareSdrAudioTransition();
  }
  const payload = filterOnly ? { cmd: 'SET_SDR_CHANNEL', channel: 1, mode, ifbw } : { cmd: 'SET_SDR_CHANNEL', channel: 1, enabled: true, mode, ifbw, volume, squelch,
    agc: sdrAgc.get(), am_sync: sdrAmSync.get(), deemph_us: deemph, nr: sdrNr.get(),
    nr_strength: sdrNrStrength.get(), nr_algorithm: sdrNrAlgo.get(), nr_atten_db: sdrNrAtten.get(),
    sstv_mode: sdrSstvMode.get() < 0 ? null : sdrSstvMode.get() };
  if (receiverInFlight) {
    receiverQueued = { payload, mode, ifbw, restoringMode };
  } else if (sdrDemod.pending() || sdrIfbw.pending()) {
    sendReceiverEdit({ payload, mode, ifbw, restoringMode });
  } else send(payload);
}

/** Noise reduction is applied by the backend receiver. */
export function toggleSdrNr() {
  if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
  const on = !sdrNr.get();
  sdrNr.set(on);
  sendSdrNr();
  renderSdrState();
}

/** How hard the reducer pushes (0..1), applied live. */
export function setSdrNrStrength(strength: number) {
  if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
  if (!Number.isFinite(strength)) return;
  sdrNrStrength.set(strength);
  sendSdrNr();
  renderSdrState();
}

/** Select the receiver's noise reduction algorithm. */
export function setSdrNrAlgo(algo: NrAlgo) {
  if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
  sdrNrAlgo.set(algo);
  sendSdrNr();
  renderSdrState();
}

/** DeepFilterNet3's attenuation limit in dB, applied live. */
export function setSdrNrAtten(atten: number) {
  if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
  if (!Number.isFinite(atten)) return;
  sdrNrAtten.set(atten);
  sendSdrNr();
  renderSdrState();
}

function sendSdrNr(): void {
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, nr: sdrNr.get(), nr_strength: sdrNrStrength.get(),
         nr_algorithm: sdrNrAlgo.get(), nr_atten_db: sdrNrAtten.get() });
}

let agcSerial = 0;
let pendingAgc: { requestId: string; enabled: boolean; timer: number } | null = null;

function renderSdrAgc(): void {
  const button = document.getElementById('btn-sdr-agc');
  if (!button) return;
  const on = sdrAgc.get();
  button.classList.toggle('active', on);
  button.setAttribute('aria-pressed', String(on));
  button.dataset.i18n = on ? 'agc_on' : 'agc_off';
  button.textContent = t(on ? 'agc_on' : 'agc_off');
}

export function resetSdrAgcRequest(): void {
  if (pendingAgc) window.clearTimeout(pendingAgc.timer);
  pendingAgc = null;
  sdrAgc.reset();
  renderSdrAgc();
}

export function rejectSdrAgcRequest(responseTo?: string, requestId?: string | number): void {
  if (responseTo === 'SET_SDR_CHANNEL' && pendingAgc && pendingAgc.requestId === requestId) resetSdrAgcRequest();
}

function toggleSdrAgc() {
  if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
  const enabled = !sdrAgc.get();
  if (pendingAgc) window.clearTimeout(pendingAgc.timer);
  sdrAgc.set(enabled);
  const requestId = `sdr-agc-${++agcSerial}`;
  pendingAgc = { requestId, enabled, timer: window.setTimeout(() => {
    if (pendingAgc?.requestId === requestId) resetSdrAgcRequest();
  }, 3000) };
  renderSdrAgc();
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, agc: enabled, request_id: requestId });
}

// Band presets (centre, capture bandwidth, demod, IF bandwidth)
const SDR_BANDS: Record<string, { center: number; decimate: number; demod: string; ifbw: number }> = {
  fm: { center: 98e6, decimate: 2, demod: 'wfm', ifbw: 180000 },
  air: { center: 127.5e6, decimate: 16, demod: 'am', ifbw: 25000 },
  vhf: { center: 145e6, decimate: 16, demod: 'nfm', ifbw: 12000 },
  uhf: { center: 435e6, decimate: 16, demod: 'nfm', ifbw: 12000 },
};

export function applySdrBand(name: string) {
  const b = SDR_BANDS[name];
  if (!b) return;
  cancelSdrTuneRequest();
  cancelSdrTuningEdit();
  cancelSdrCanvasGesture();
  prepareSdrAudioTransition();
  sdrCenterHz.set(b.center);
  sdrDecimate.set(b.decimate);
  sdrSpanHz.set(estimatedCaptureSpanHz(b.decimate)); // estimate until the device reports the real span
  sdrDemod.set(b.demod);
  sdrIfbw.set(b.ifbw);
  sdrListenHz.set(b.center);
  renderSdrState();
  renderSdrTuningUi();
  send({ cmd: 'SET_SDR', center: b.center, decimate: b.decimate });
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: b.center });
  applySdrDemod();
}

export function listenAtFreq(hz: number) {
  if (!isFinite(hz) || hz <= 0) return;
  if (currentGraphMode() === 'sdr') {
    requestSdrTune(hz);
    return;
  }
  // From the swept view: hand this frequency to SDR for demodulation. BOTH the capture centre and
  // the listen frequency go there - setting only the centre left the previous listen frequency in
  // place, so the backend re-centred the capture to chase a channel the user never asked for
  // (measured: Shift+click at 216 MHz landed SDR at 987 MHz).
  sdrCenterHz.set(hz);
  sdrListenHz.set(hz);
  setGraphMode('sdr');
}

function syncSdrButtons() {
  const mode = sdrDemod.get();
  document.querySelectorAll('[data-sdr-demod]').forEach((el) => {
    el.classList.toggle('active', (el as HTMLElement).dataset.sdrDemod === mode);
  });
  const drm = mode === 'drm' || mode === 'drmplus';
  for (const id of ['btn-sdr-agc', 'input-sdr-squelch', 'btn-sdr-nr',
    'select-sdr-nr-algo', 'select-sdr-nr-strength', 'input-sdr-nr-atten']) {
    const control = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | HTMLButtonElement | null;
    if (control) control.disabled = drm || (id === 'select-sdr-nr-strength' && sdrNrAlgo.get() === 'dfn');
  }
  renderSdrFilter();
  const dv = sdrDeemph.get();
  document.querySelectorAll<HTMLButtonElement>('[data-sdr-deemph]').forEach((el) => {
    el.disabled = drm;
    el.classList.toggle('active', Number((el as HTMLElement).dataset.sdrDeemph) === dv);
  });
}

// ── SDR audio enable + amplitude reference ──

let filterSerial = 0;
type ReceiverEdit = { payload: Record<string, unknown>; mode: string; ifbw: number; restoringMode: boolean };
let receiverInFlight: (ReceiverEdit & { requestId: string; timer: number }) | null = null;
let receiverQueued: ReceiverEdit | null = null;
let receiverConfigVersion = -1;

function clearReceiverEdit(): void {
  if (receiverInFlight) window.clearTimeout(receiverInFlight.timer);
  receiverInFlight = null;
  receiverConfigVersion = -1;
  receiverQueued = null;
}

function sendReceiverEdit(edit: ReceiverEdit): void {
  const requestId = `sdr-filter-${++filterSerial}`;
  receiverInFlight = { ...edit, requestId, timer: window.setTimeout(() => {
    if (receiverInFlight?.requestId !== requestId) return;
    finishReceiverEdit(false);
    S.setStatusError(t('sdr_filter_timeout'));
  }, 3000) };
  const liveAudio = 'volume' in edit.payload ? {
    volume: sdrVolume.get(), squelch: sdrSquelch.get(), agc: sdrAgc.get(), am_sync: sdrAmSync.get(),
    deemph_us: sdrDeemph.get(), nr: sdrNr.get(), nr_strength: sdrNrStrength.get(),
    nr_algorithm: sdrNrAlgo.get(), nr_atten_db: sdrNrAtten.get(),
    sstv_mode: sdrSstvMode.get() < 0 ? null : sdrSstvMode.get(),
  } : {};
  send({ ...edit.payload, ...liveAudio, request_id: requestId });
}

function finishReceiverEdit(accepted: boolean): void {
  if (receiverInFlight) window.clearTimeout(receiverInFlight.timer);
  receiverInFlight = null;
  const next = receiverQueued;
  receiverQueued = null;
  if (next) {
    sdrDemod.set(next.mode);
    sdrDemod.touch(next.mode);
    sdrIfbw.set(next.ifbw);
    sdrIfbw.touch(next.ifbw);
    sendReceiverEdit(next);
  } else {
    sdrDemod.reset();
    sdrIfbw.reset();
    if (!accepted) cancelSdrCanvasGesture();
  }
  setSdrAudioMode(1, sdrDemod.get());
  renderSdrState();
  syncSdrButtons();
  requestRender();
}

export function rejectSdrFilterRequest(responseTo?: string, requestId?: string | number, receiverFailed = false): void {
  const rejected = receiverInFlight;
  if (receiverFailed && responseTo === undefined) {
    receiverQueued = null;
    if (rejected) finishReceiverEdit(false);
    return;
  }
  if (responseTo !== 'SET_SDR_CHANNEL' || !rejected || rejected.requestId !== requestId) return;
  if (receiverFailed) { receiverQueued = null; finishReceiverEdit(false); return; }
  if (!receiverQueued && rejected.restoringMode && rejected.ifbw !== filterSpec(rejected.mode).defaultHz) {
    sdrDemod.set(rejected.mode);
    sdrIfbw.set(filterSpec(rejected.mode).defaultHz);
    receiverQueued = { mode: rejected.mode, ifbw: filterSpec(rejected.mode).defaultHz, restoringMode: false,
      payload: { ...rejected.payload, ifbw: filterSpec(rejected.mode).defaultHz } };
  }
  finishReceiverEdit(false);
}

/** The same user selection action serves the registry buttons and the M shortcut. */
export function selectSdrDemod(id: string): void {
  cancelSdrTuneRequest();
  cancelSdrCanvasGesture();
  cancelSdrTuningEdit();
  clearPassbandView();
  cancelSdrFilterEdit();
  sdrDemod.set(id);
  sdrIfbw.set(filterValueForMode(id));
  renderSdrState();
  renderSdrTuningUi();
  syncSdrButtons();
  applySdrDemod(true, true);
}

/** Both click and keyboard bandwidth choices are explicit manual choices. */
export function selectSdrIfbw(ifbw: number): void {
  const spec = filterSpec(sdrDemod.get());
  if (!spec.editable || !Number.isFinite(ifbw) || ifbw < spec.minimumHz) return;
  sdrIfbw.set(ifbw);
  renderSdrState();
  syncSdrButtons();
  applySdrDemod(false, true);
  requestRender();
}

function syncSdrAudioButton() {
  const b = document.getElementById('btn-sdr-audio');
  if (b) {
    b.textContent = sdrAudioOn.get() ? t('on') : t('off');
    b.classList.toggle('active', sdrAudioOn.get());
  }
}

function applySdrAudioPreference() {
  const on = sdrAudioOn.get();
  sdrAudioOn.set(on);
  setSdrAudioMode(1, sdrDemod.get());
  setSdrAudioEnabled(on);
  syncSdrAudioButton();
}

export function toggleSdrAudio() {
  const on = !sdrAudioOn.get();
  sdrAudioOn.set(on); // the slot persists it (single writer)
  setSdrAudioMode(1, sdrDemod.get());
  setSdrAudioEnabled(on);
  syncSdrAudioButton();
}

// SDR status -> panel readouts (called on every STATUS)
function sdrSet(id: string, value: string) {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  if (el && document.activeElement !== el) el.value = value;
}

// Live SDR geometry for click/drag/wheel interaction (updated from STATUS).
export function syncSdrPanel(s: any) {
  const sdr = s?.sdr;
  const active = s?.connected === true && s?.mode === 'sdr';
  if (s?.connected !== true) {
    disconnectSdrSession();
    return;
  }
  setDecodeActive(active);
  if (!active) {
    if (canvasGesture?.sdr) cancelSdrCanvasGesture();
    cancelSdrTuningEdit();
    clearPassbandView();
    sdrFilterLimits.confirm(null);
    clearReceiverEdit();
    resetSdrTuneSession();
    resetSdrAgcRequest();
    cancelSdrFilterEdit();
    sdrDemod.reset();
    sdrIfbw.reset();
    confirmAmSyncGeneration(null);
    setSdrIqEnabled(false);
    return;
  }
  setSdrIqEnabled(true);
  if (!sdr) {
    confirmAmSyncGeneration(null);
    return;
  }
  const a = sdr.actual || {};
  const channels = Array.isArray(sdr.channels) ? sdr.channels : [];
  const primary = channels.find((channel: { channel: number }) => channel.channel === 1);
  const receiver = primary || sdr;
  const ownReply = typeof s.request_id === 'string' && s.request_id.startsWith('sdr-filter-');
  const version = Number(s.config_version);
  if ((ownReply && s.request_id !== receiverInFlight?.requestId)
      || (Number.isFinite(version) && version < receiverConfigVersion)) return;
  if (!confirmSdrTuneReport(Number(primary ? primary.freq : sdr.listen), s.response_to, s.request_id, s.config_version)) return;
  const generation = primary?.am_sync_generation;
  confirmAmSyncGeneration(typeof generation === 'number' && Number.isInteger(generation)
    && generation >= 0 && generation <= 0xffffffff ? generation : null);
  // The saved detector preference is replayed only once, when a connected SDR STATUS
  // first arrives; a disconnected mode report is not an entry into a receiver session.
  if (initialSdrAmSync !== null) {
    const enabled = initialSdrAmSync;
    initialSdrAmSync = null;
    requestSdrAmSync(enabled);
  }
  // Confirm the tuning group from the backend, then render it from the slots (the only
  // writer of those controls). The demod group still uses the DOM for now.
  sdrCenterHz.confirm(Number(sdr.center) || 0);
  sdrDecimate.confirm(Number(sdr.decimate) || 32);
  if (a.start != null && a.stop != null) sdrSpanHz.confirm(Number(a.stop) - Number(a.start));
  const displaySpan = a.start != null && a.stop != null
    ? Number(a.stop) - Number(a.start) : estimatedCaptureSpanHz(Number(sdr.decimate) || 32);
  confirmSdrAxisWindow(Number(sdr.center), displaySpan, Number(s.config_version));
  const actualMode = String((primary ? primary.mode : sdr.demod) || 'am');
  const actualFilter = Number(receiver.if_bw) || 6000;
  if (Number.isFinite(version)) receiverConfigVersion = version;
  if (actualMode !== sdrDemod.confirmedValue()) {
    clearPassbandView();
    cancelSdrCanvasGesture();
    cancelSdrFilterEdit();
    resetCarrierEvidence();
  } else if (receiver.pitch != null && receiver.pitch !== sdrPitchHz.get()) { cancelSdrCanvasGesture(); resetCarrierEvidence(); }
  sdrDemod.confirm(actualMode);
  sdrIfbw.confirm(actualFilter);
  const limits = sdr.filter_limits;
  sdrFilterLimits.confirm(limits && limits.mode === actualMode && Number.isFinite(limits.min_hz)
    && (limits.max_hz === null || Number.isFinite(limits.max_hz)) ? limits : null);
  rememberConfirmedFilter(actualMode, actualFilter);
  if (receiverInFlight) {
    const ack = s.response_to === 'SET_SDR_CHANNEL' && s.request_id === receiverInFlight.requestId;
    if (ack) finishReceiverEdit(true);
    else {
      const target = receiverQueued ?? receiverInFlight;
      sdrDemod.set(target.mode);
      sdrIfbw.set(target.ifbw);
      sdrDemod.touch(target.mode);
      sdrIfbw.touch(target.ifbw);
    }
  }
  setSdrAudioMode(1, sdrDemod.get());
  renderSdrTuningUi();
  if (receiver.pitch != null) sdrPitchHz.confirm(Number(receiver.pitch));
  if ('sstv_mode' in receiver) sdrSstvMode.confirm(receiver.sstv_mode == null ? -1 : Number(receiver.sstv_mode));
  if (typeof primary?.am_sync === 'boolean') {
    sdrAmSync.confirm(primary.am_sync);
    if (!sdrAmSync.pending()) clearAmSyncRequest();
  }
  refreshDecodeWindow();
  if (receiver.deemph_us != null) sdrDeemph.confirm(Number(receiver.deemph_us));
  if (receiver.volume != null) sdrVolume.confirm(Number(receiver.volume));
  if (receiver.squelch != null) sdrSquelch.confirm(Number(receiver.squelch));
  const agcReply = typeof s.request_id === 'string' && s.request_id.startsWith('sdr-agc-');
  if (typeof receiver.agc === 'boolean' && (!agcReply || s.request_id === pendingAgc?.requestId)) {
    sdrAgc.confirm(receiver.agc);
    if (pendingAgc) {
      if (s.response_to === 'SET_SDR_CHANNEL' && s.request_id === pendingAgc.requestId) resetSdrAgcRequest();
      else sdrAgc.set(pendingAgc.enabled);
    }
  }
  if (receiver.nr != null) sdrNr.confirm(!!receiver.nr);
  if (receiver.nr_strength != null) sdrNrStrength.confirm(Number(receiver.nr_strength));
  if (receiver.nr_algorithm != null) sdrNrAlgo.confirm(receiver.nr_algorithm === 'dfn' ? 'dfn' : 'wiener');
  if (receiver.nr_atten_db != null) sdrNrAtten.confirm(Number(receiver.nr_atten_db));
  for (const channel of channels) {
    if (channel.channel === 2 || channel.channel === 3) {
      setDecodeChannelMode(channel.channel, channel.enabled ? channel.mode : '');
    }
  }
  renderSdrState();
  // The demod group's controls are projections of the slots (the slots own the values, so a
  // preference restored from storage cannot disagree with the form).
  sdrSet('input-sdr-volume', String(sdrVolume.get()));
  // Squelch and its dBm readout are rendered by renderSdrState, including fractional dB.
  renderSdrAgc();
  const lvl = document.getElementById('cur-sdr-level');
  if (lvl) {
    const value = primary ? primary.level_dbfs : sdr.level_dbfs;
    lvl.textContent = typeof value === 'number' && Number.isFinite(value)
      ? value.toFixed(1) + ' dBFS' : '';
    lvl.removeAttribute('title');
  }
  syncSdrButtons();
  syncSdrAudioButton();
  syncRefUI();
  setDecodeChannelMode(1, primary && !primary.enabled ? '' : sdrDemod.get());
}
// 仅 ×N(6)/Manual(7) 需要输入框+Set 按钮; 其余固定档隐藏
// Turn all markers on/off at once (toggle)
// Preset
let presetSerial = 0;
let pendingPreset: { requestId: string; timer: number } | null = null;

export function presetAll() {
  cancelSdrTuneRequest();
  cancelSdrTuningEdit();
  resetAxisPreview();
  clearPassbandView();
  sdrFilterLimits.confirm(null);
  clearReceiverEdit();
  cancelSdrCanvasGesture();
  cancelSdrFilterEdit();
  sdrDemod.reset();
  sdrIfbw.reset();
  if (pendingPreset) window.clearTimeout(pendingPreset.timer);
  const requestId = `acq-preset-${++presetSerial}`;
  pendingPreset = { requestId, timer: window.setTimeout(() => { pendingPreset = null; }, 3000) };
  send({ cmd: 'SET_PRESET', request_id: requestId });
}

export function rejectPresetRequest(responseTo?: string, requestId?: string | number) {
  const pending = pendingPreset;
  if (responseTo !== 'SET_PRESET' || !pending || pending.requestId !== requestId) return;
  window.clearTimeout(pending.timer);
  pendingPreset = null;
}

export function syncPresetStatus(responseTo?: string, requestId?: string | number) {
  const pending = pendingPreset;
  if (responseTo !== 'SET_PRESET' || !pending || pending.requestId !== requestId) return false;
  window.clearTimeout(pending.timer);
  pendingPreset = null;
  applyPresetSuccess();
  return true;
}

function applyPresetSuccess() {
  resetAxisPreview();
  resetCarrierEvidence();
  resetFrequencyEditors();
  resetResolutionEditors();
  resetSweepEditor();
  resetRefClockIntent();
  resetAmpIntents();
  resetAll('swp');
  resetAll('rta');
  resetAll('ref');
  resetTraceFamilyModes();
  setScale(10);
  resetTrigger();
  exitMeasModePub();
  S.setMeasOn(false);
  const b = document.getElementById('btn-meas-onoff');
  if (b) b.textContent = t('off');
  setMeasButtons(false);
  // SDR keeps the raw display window until the one-shot fit returns; compensation resets now.
  commitLevelOffset(0);
  if (currentGraphMode() !== 'sdr') setDisplayRef('preset', 0);
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of) of.value = '0';
  const presetTraceMode = currentGraphMode() === 'sdr' ? 'AVERAGE' : 'CLEAR_WRITE';
  S.traces.forEach((t, i) => {
    t.mode = i === 0 ? presetTraceMode : 'OFF'; t.reference = null; t.isNormalized = false;
    t.avgSum = null; t.avgCount = 0; t.avgTarget = 16; t.avgTargetRta = SDR_DEFAULT_AVG; t.prevMode = undefined; t.done = false;
  });
  clearRtaAccum();
  // The trace panel must follow the state back to the factory defaults (reported: after a
  // Preset the panel still showed the last trace's Mode/Avg/Freeze and the Smooth/RefWin
  // selects kept their old values, while the traces themselves were already reset - the
  // display and the state disagreed until the next reload). switchTraceTab's display-side
  // effects are deliberately NOT reused here: it would re-derive the display ref from the
  // not-yet-reset ref level; the panel widgets are synced directly instead.
  S.setActiveTraceIdx(0);
  document.querySelectorAll('.trace-btn').forEach((b, i) => b.classList.toggle('active', i === 0));
  const modeSel = document.getElementById('select-trace-mode') as HTMLSelectElement | null;
  if (modeSel) modeSel.value = presetTraceMode;
  syncFreezeBtn();
  syncAvgUI();
  updateNormalizeStatusUI();
  const smoothSel = document.getElementById('select-smooth') as HTMLSelectElement | null;
  if (smoothSel) smoothSel.value = '1';
  const refwinSel = document.getElementById('select-refwin') as HTMLSelectElement | null;
  if (refwinSel) refwinSel.value = '0';
  normRefWinUser.set(0);
  S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; });
  updateMarkersAllBtn();
  S.setM3dB(null); S.setAmpRes(null); S.setHarm(null); S.setPnmData(null);
  S.setPnmCarAcc(null);
  clearChannel();
  peakListVisible.set(false); S.setPeakMarks(null);
  resetPeakThr();
  const pl = document.getElementById('btn-peaklist');
  if (pl) pl.textContent = t('off');
  S.setActiveMkrId(1);
  syncMarkerTrackingToggle();
  // Preset also resets the RTA session (backend reset_defaults) and clears the RTA
  // memory + UI so re-entering RTA starts from factory defaults.
  if (S.rtaMode) {
    const spanSel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
    if (spanSel) spanSel.value = '50781250';
    const rbwSel = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
    if (rbwSel) rbwSel.value = 'auto';
    const vbwSel = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
    if (vbwSel) vbwSel.value = 'equal';
    const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement;
    if (sm) { sm.value = '2'; syncSweepInput(); }
  }
  // Preset resets acquisition settings while keeping the active mode. Preserve its saved
  // confirmation too: a same-mode STATUS does not repeat the mode-change persistence above.
  try {
    ['web-sa-sdr-audio', 'web-sa-sdr-ref-auto'].forEach((k) => localStorage.removeItem(k));
  } catch { /* ignore */ }
  resetRtaDensityCfg();
  resetLimits();
  S.resetWaterfall();
  setWfPaused(false);
  resetWfSplit();                       // the dragged spectrum/waterfall split goes back to default
  // Preset restores acquisition defaults: the zoom view dies with the old window (§4.3).
  resetViewport();
  cancelZoomGesture();
  syncSpectrumZoomUi(true);
  smoothBins.set(1);
  spanStepAuto.set(true);
  // Preset resets the device, not the listener: the audio switch and the IQ ingress survive. The
  // backend's reconfigure re-anchors the stream with its own flush frame, so stopping the ingress
  // here only orphaned it (re-enabling audio afterwards still heard nothing until a reload).
  clearReceiverEdit();
  cancelSdrCanvasGesture();
  const audioWasOn = sdrAudioOn.get();
  resetSdrIq();

  // Every pending SDR intent (including a hand-off centre) is dropped by one call - the old
  // code cleared the fields by hand and missed one, so a Preset could reapply the previous
  // frequency.
  initialSdrNr = null;
  initialSstvMode = null;
  initialSdrAmSync = null;
  clearAmSyncRequest();
  resetSdrAgcRequest();
  resetSdrState();
  sdrAudioOn.set(audioWasOn);
  renderSdrState();
  refreshDecodeWindow();
  resetAutoScaleState();               // the reference is about to be reset: forget the last fit
  // A preset makes the SDR level-less again (`sdr.ref_set` false). That is the state the entry
  // fit exists for, so arm it. The display keeps its scale until the placement answers.
  if (currentGraphMode() === 'sdr') requestSdrEntryFit(false);
  setSdrAudioEnabled(audioWasOn);
  syncSdrAudioButton();
  updateInfoBar(); applyMeasUI(); requestRender();
}

// Current frontend time (shown when not locked)
// GNSS detail popover: fill + show/close
// Sync all toggle button texts when the language changes
// ── Panel collapse ──
// ── data-action binding ──
export function bindActions() {
  document.addEventListener('pointerdown', (event) => {
    if (!(event.target instanceof Element)) return;
    const takeover = event.target.closest('#sdr-demod-group [data-sdr-demod], #btn-mode-sdr, #btn-mode-rta, [data-action="preset"], [data-sdr-band]');
    if (takeover && !takeover.matches(':disabled')) cancelSdrTuningEdit();
  }, true);
  loadCommandPolicy();
  initPeakCountUi();
  bindSdrTuningUi({
    onStepCommit: (tier, hz) => { setSdrStepForCurrentMode(hz, tier); renderSdrState(); renderSdrTuningUi(); },
    onLocateSelect: (mode) => { sdrLocateMode.set(mode); renderSdrTuningUi(); },
    onGridCommit: (stepHz, anchorHz) => { sdrChannelStepHz.set(stepHz); sdrChannelAnchorHz.set(anchorHz); renderSdrTuningUi(); },
  });
  const act: Record<string, (el: HTMLElement) => void> = {
    'apply-center-span': () => applyCenterSpan(),
    'apply-start-stop': () => applyStartStop(),
    'full-span': () => applyFullSpan(),
    'swp-span-down': () => stepSwpSpan(-1),
    'swp-span-up': () => stepSwpSpan(1),
    'span-step-auto': () => resetSpanStepAuto(),
    'set-ref-level': () => setRefLevel(),
    'set-ref-auto': () => setRefAuto(),
    'ref-down': () => adjustRefLevel(-1),
    'ref-up': () => adjustRefLevel(1),
    'apply-rbw': () => applyRBW(),
    'apply-vbw': () => applyVBW(),
    'apply-points': () => applyPoints(),
    'set-window': (el) => setWindow((el as HTMLSelectElement).value),
    'set-spur': (el) => setSpurMode((el as HTMLSelectElement).value),
    'set-detector': (el) => send({ cmd: 'SET_DETECTOR', mode: (el as HTMLSelectElement).value }),
    'set-framedet': (el) => setFrameDetector((el as HTMLSelectElement).value),
    'set-trace-align': (el) => setTraceAlign((el as HTMLSelectElement).value),
    'set-points-strategy': (el) => setPointsStrategy((el as HTMLSelectElement).value),
    'set-refclock': (el) => setRefClock((el as HTMLSelectElement).value),
    'toggle-refclkout': () => toggleRefClkOut(),
    'set-amp': (el) => setAmp(el as HTMLSelectElement),
    'set-trace-mode': (el) => setTraceMode((el as HTMLSelectElement).value),
    'set-trace-avg': (el) => setTraceAverage(parseInt((el as HTMLSelectElement).value) || 0),
    'export-csv': () => exportActiveTraceCsv(),
    'export-png': () => exportSpectrumPng(),
    'export-peaks-csv': () => exportPeakListCsv(),
    'set-smooth': (el) => { smoothBins.set(parseInt((el as HTMLSelectElement).value) || 1); requestRender(); },
    'set-norm-refwin': (el) => setNormalizeReferenceWindow(parseInt((el as HTMLSelectElement).value) || 0),
    'toggle-freeze': () => toggleFreeze(),
    'normalize': () => normalizeActiveTrace(),
    'reset-norm': () => resetActiveTraceNormalize(),
    'clear-trace': () => {
      if (S.rtaMode) { clearRtaTrace(); }
      else resetTraceAccum(S.traces[S.activeTraceIdx]);
    },
    'mkr-peak': () => activeMarkerPeak(),
    'mkr-peak-left': () => activeMarkerNextPeakLeft(),
    'mkr-peak-right': () => activeMarkerNextPeakRight(),
    'mkr-valley': () => activeMarkerValley(),
    'mkr-valley-left': () => activeMarkerNextValleyLeft(),
    'mkr-valley-right': () => activeMarkerNextValleyRight(),
    'mkr-center': () => markerToCenter(),
    'peakthr-auto': () => peakThrAuto(),
    'toggle-peaklist': () => togglePeakList(),
    'meas-toggle': () => measToggle(),
    'meas-amp': () => measureAmp(),
    'clear-amp': () => clearAmp(),
    'meas-chan': () => measureChannel(),
    'clear-chan': () => clearChannel(),
    'meas-harm': () => measHarmApply(),
    'meas-pnm': () => measPnmApply(),
    'connect': () => connectDevice(),
    'gnss-detail': () => fillGnssDetail(),
    'gnss-close': () => closeGnssDetail(),
    'device-detail': () => fillDeviceDetail(),
    'device-close': () => closeDeviceDetail(),
    'refclk-detail': () => openRefClockDetail(),
    'refclk-close': () => closeRefClockDetail(),
    'markers-all': () => toggleMarkersAll(),
    'marker-tracking': () => toggleActiveMarkerTracking(),
    'set-sweep': () => { syncSweepInput(); setSweepSpeed(); },
    'toggle-rta': () => setGraphMode(currentGraphMode() === 'rta' ? 'swp' : 'rta'),
    'apply-rta': () => applyRta(),
    'toggle-sdr': () => setGraphMode(currentGraphMode() === 'sdr' ? 'swp' : 'sdr'),
    'apply-sdr': () => applySdr(),
    'apply-sdr-bw': () => applySdrBw(),
    'apply-sdr-tune': () => applySdrTune(),
    'sdr-tune-down': () => sdrTuneBy(-currentSdrStepHz()),
    'sdr-tune-up': () => sdrTuneBy(currentSdrStepHz()),
    'sdr-to-center': () => sdrToCenter(),
    'set-sdr-demod': () => applySdrDemod(),
    'toggle-sdr-agc': () => toggleSdrAgc(),
    'toggle-sdr-nr': () => toggleSdrNr(),
    'toggle-decode-window': () => toggleDecodeWindow(),
    'set-sdr-nr-strength': (el) => setSdrNrStrength(Number((el as HTMLSelectElement).value)),
    'set-sdr-nr-algo': (el) => setSdrNrAlgo((el as HTMLSelectElement).value === 'dfn' ? 'dfn' : 'wiener'),
    'set-sdr-nr-atten': (el) => setSdrNrAtten(Number((el as HTMLInputElement).value)),
    'toggle-sdr-audio': () => toggleSdrAudio(),
    'rta-span-down': () => rtaSpanStep(1),
    'rta-span-up': () => rtaSpanStep(-1),
    'rta-span-full': () => rtaSpanFull(),
    'set-rta-fade': (el) => setRtaHalflife(parseFloat((el as HTMLSelectElement).value)),
    'set-rta-bins': (el) => setRtaBins(parseInt((el as HTMLSelectElement).value) || 128),
    'set-rta-dens-source': (el) => setRtaDensSource((el as HTMLSelectElement).value === 'live' ? 'live' : 'trace'),
    'set-rta-dens-layers': (el) => setRtaDensLayers((el as HTMLSelectElement).value as 'both' | 'density' | 'trace'),
    'set-rta-dens-contrast': (el) => setRtaDensContrast(parseFloat((el as HTMLSelectElement).value)),
    'set-rta-dens-width': (el) => setRtaDensWidth(Number((el as HTMLSelectElement).value)),
    'toggle-density-settings': () => toggleDensitySettings(),
    'wf-pause': () => toggleWfPause(),
    'wf-reset': () => resetWf(),
    'preset': () => presetAll(),
    'toggle-gapfill': () => toggleGapFill(),
    'toggle-group': (el) => toggleGroup(el),
    'toggle-all-groups': () => toggleAllGroups(),
  };
  document.querySelectorAll('[data-action]').forEach(el => {
    const a = el.getAttribute('data-action')!;
    const handler = act[a];
    if (!handler) return;
    if (el.tagName === 'SELECT') {
      (el as HTMLSelectElement).addEventListener('change', () => handler(el as HTMLElement));
    } else {
      el.addEventListener('click', () => handler(el as HTMLElement));
    }
    // Non-native activatable elements (panel headers, info readouts) get button
    // semantics: keyboard focus + Enter/Space activation, matching the click handler.
    if (!['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'A', 'LABEL'].includes(el.tagName)) {
      (el as HTMLElement).tabIndex = 0;
      el.setAttribute('role', 'button');
      (el as HTMLElement).addEventListener('keydown', (ev: KeyboardEvent) => {
        if ((ev.key !== 'Enter' && ev.key !== ' ') || ev.ctrlKey || ev.metaKey || ev.altKey) return;
        ev.preventDefault();
        handler(el as HTMLElement);
      });
    }
  });
  document.addEventListener('websa:unit-commit', (event) => {
    const detail = (event as CustomEvent<{ field: string; commit: boolean }>).detail;
    // The step box is not a panel field; apply it here instead of in commitUnitField.
    if (detail.field === 'sdr_step' && detail.commit) applySdrStep();
    commitUnitField(detail.field, detail.commit);
  });

  const selectOnFocus = [
    'input-center', 'input-span', 'input-start', 'input-stop', 'input-rta-center',
    'input-rbw', 'input-vbw', 'input-pnm', 'input-span-step',
    'input-sdr-center', 'input-sdr-listen',
  ];
  for (const id of selectOnFocus) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    input?.addEventListener('focus', () => requestAnimationFrame(() => input.select()));
  }

  // SDR demod panel: ranges commit on change (not on every drag pixel), and the
  // frequency inputs commit on Enter.
  const volumeEl = document.getElementById('input-sdr-volume') as HTMLInputElement | null;
  volumeEl?.addEventListener('change', () => {
    // A slider at its left end means 0, which is silence - `x || 0.8` read that 0 as "unset" and put
    // the volume back to 0.8, so the listener heard the noise floor at nearly full level, louder than
    // at the settings just beside it, and no amount of turning the level down reached silence.
    const wanted = Number.parseFloat(volumeEl.value);
    sdrVolume.set(Number.isFinite(wanted) ? Math.max(0, Math.min(2, wanted)) : 0.8);
    applySdrDemod();
  });
  const squelchEl = document.getElementById('input-sdr-squelch') as HTMLInputElement | null;
  squelchEl?.addEventListener('input', () => {
    const label = document.getElementById('cur-sdr-squelch');
    if (label) label.textContent = Number(squelchEl.value) <= -160 ? t('off') : `${Number(squelchEl.value).toFixed(1)} dBm`;
  });
  squelchEl?.addEventListener('change', () => {
    if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
    const wanted = Number.parseFloat(squelchEl.value);
    sdrSquelch.set(Number.isFinite(wanted) ? wanted : -110);
    applySdrDemod();
  });
  // The demod group is generated from the DSP plugin registry, so the mode buttons and the
  // kernels cannot disagree; the handler is the same path the panel always used.
  // The FT8 decode window: its toggle lives next to the readout, its rows come from the log, and a
  // row click tunes the receiver (the only action an FT8 operator takes on a decode).
  // A row tunes the *dial* so the signal lands inside the decoder's band (see `ft8DialFor`).
  initDecodeWindow({
    onTune: (hz) => {
      const freq = ft8DialFor(hz);
      const channel = decodeChannel();
      if (channel === 1) listenAtFreq(freq);
    },
    onSstvMode: (index) => {
      if (deviceMode.get() !== 'sdr') return;
      sdrSstvMode.set(index ?? -1);
      setSstvModePin(index, 1);
      refreshDecodeWindow();
    },
    canAmSync: amSyncSessionAvailable,
    onAmSync: (enabled) => {
      if (!amSyncSessionAvailable() || !canToggleAmSync() || sdrDemod.confirmedValue() !== 'am') return;
      requestSdrAmSync(enabled);
    },
  });
  const demodGroup = document.getElementById('sdr-demod-group');
  if (demodGroup) {
    void initSdrDemodGroup(demodGroup, {
      onSelect: selectSdrDemod,
    });
  }
  bindSdrFilter({ onCommit: selectSdrIfbw, onView: viewSdrPassband });
  document.querySelectorAll('[data-sdr-deemph]').forEach((el) => {
    el.addEventListener('click', () => {
      if (sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus') return;
      const value = Number((el as HTMLElement).dataset.sdrDeemph ?? -1);
      sdrDeemph.set(value);
      renderSdrState();
      applySdrDemod();
    });
  });
  document.querySelectorAll('[data-sdr-band]').forEach((el) => {
    el.addEventListener('click', () => applySdrBand((el as HTMLElement).dataset.sdrBand || 'fm'));
  });
  const sdrListenEl = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (sdrListenEl) sdrListenEl.addEventListener('change', () => applySdrTune());
  const sdrCenterEl = document.getElementById('input-sdr-center') as HTMLInputElement | null;
  if (sdrCenterEl) sdrCenterEl.addEventListener('change', () => applySdr());
  // Persisted SDR preferences are restored by the slots themselves (core/params.ts).
  syncSdrAudioButton();
  syncRefUI();
  const sdrCenter = document.getElementById('input-sdr-center') as HTMLInputElement | null;
  if (sdrCenter) sdrCenter.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdr();
  });
  const sdrListen = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (sdrListen) sdrListen.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdrTune();
  });
  // The step box: the shared unit buttons (unit-sdr_step-group, built by buildUnitGroups)
  // convert the value and report websa:unit-commit; Enter and change apply it. One writer
  // (setSdrStepForCurrentMode) and one render path (renderSdrState), like every other SDR
  // control.
  const sdrStepEl = document.getElementById('input-sdr-step') as HTMLInputElement | null;
  if (sdrStepEl) {
    sdrStepEl.addEventListener('input', () => { sdrStepEl.dataset.edited = '1'; });
    sdrStepEl.addEventListener('change', () => applySdrStep());
    sdrStepEl.addEventListener('keydown', (ev) => {
      if ((ev as KeyboardEvent).key === 'Enter') { ev.preventDefault(); applySdrStep(); }
    });
  }
  const sdrStepQuick = document.getElementById('sdr-step-quick');
  if (sdrStepQuick) {
    for (const hz of SDR_STEP_QUICK_HZ) {
      const btn = document.createElement('button');
      btn.className = 'btn';
      btn.type = 'button';
      btn.dataset.stepHz = String(hz);
      btn.textContent = hz >= 1e6 ? `${hz / 1e6}M` : hz >= 1e3 ? `${hz / 1e3}k` : String(hz);
      btn.title = `${hz.toLocaleString('en-US')} Hz`;
      btn.addEventListener('click', () => { setSdrStepForCurrentMode(hz); renderSdrState(); renderSdrTuningUi(); });
      sdrStepQuick.appendChild(btn);
    }
  }

  // Special bindings: trace tab / meas tab / marker select / scale / peakthr input
  document.querySelectorAll('[data-trace-tab]').forEach(el => {
    el.addEventListener('click', () => switchTraceTab(parseInt((el as HTMLElement).dataset.traceTab || '0')));
  });
  document.querySelectorAll('[data-meas-tab]').forEach(el => {
    el.addEventListener('click', () => measTab((el as HTMLElement).dataset.measTab || 'amp'));
  });

  const wfBtn = document.getElementById('btn-waterfall');
  if (wfBtn) wfBtn.addEventListener('click', () => toggleWaterfall());
  // The spectrum/waterfall divider: it restores the remembered split and follows the toggle.
  initWfSplit();
  document.querySelectorAll('[data-marker-select]').forEach(el => {
    el.addEventListener('click', () => selectMarker(parseInt((el as HTMLElement).dataset.markerSelect || '1')));
  });
  // The marker table's active-arrow dispatches this so the table can switch the active
  // marker without importing controls (and syncs the right-hand Marker buttons).
  document.addEventListener('websa:marker-active', (event) => {
    const id = (event as CustomEvent<{ id: number }>).detail?.id;
    if (id) selectMarker(Number(id));
  });
  document.querySelectorAll('[data-scale]').forEach(el => {
    el.addEventListener('click', () => setScale(parseFloat((el as HTMLElement).dataset.scale || '10')));
  });
  bindSweepEditor();
  document.getElementById('select-pnm-smooth')?.addEventListener('change', repaintPnm);
  document.getElementById('input-pnm-win')?.addEventListener('input', repaintPnm);
  document.getElementById('input-pnm-win')?.addEventListener('change', repaintPnm);
  const pt = document.getElementById('input-peakthr') as HTMLInputElement;
  if (pt) {
    pt.addEventListener('input', () => peakThrManual());
    pt.addEventListener('change', () => peakThrManual());
  }
  const off = document.getElementById('input-offset') as HTMLInputElement;
  if (off) off.addEventListener('change', () => setOffset());
  ['center', 'span', 'start', 'stop'].forEach(f => {
    const el = document.getElementById(`input-${f}`) as HTMLInputElement;
    if (!el) return;
    el.addEventListener('input', () => {
      el.dataset.edited = '1';
      markFrequencyDirty('swp-freq-settings');
    });
    el.addEventListener('keydown', (event) => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      if (f === 'center' || f === 'span') applyCenterSpan();
      else applyStartStop();
    });
  });
  const rtaCenter = document.getElementById('input-rta-center') as HTMLInputElement | null;
  if (rtaCenter) {
    rtaCenter.addEventListener('input', () => {
      rtaCenter.dataset.edited = '1';
      markFrequencyDirty('rta-freq-settings');
    });
    rtaCenter.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') { event.preventDefault(); applyRta(); }
    });
  }
  for (const id of ['input-rbw', 'input-vbw', 'input-pnm']) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    input?.addEventListener('input', () => {
      input.dataset.edited = '1';
      if (id === 'input-rbw' || id === 'input-vbw') markResolutionDirty(id === 'input-rbw' ? 'rbw' : 'vbw');
    });
  }
  const spanStep = document.getElementById('input-span-step') as HTMLInputElement | null;
  if (spanStep) {
    spanStep.addEventListener('input', () => updateCustomSpanStep());
    spanStep.addEventListener('change', () => updateCustomSpanStep());
  }
  syncSwpSpanStep();
  const refInput = document.getElementById('input-ref') as HTMLInputElement | null;
  if (refInput) refInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') { event.preventDefault(); setRefLevel(); }
  });
  const hf = document.getElementById('input-harm-f0') as HTMLInputElement;
  if (hf) hf.addEventListener('input', () => autoHarmSpan());
}

// Canvas click/drag
// ── SDR canvas interaction: click to tune, drag to tune/pan ──
let sdrEdgeAt = 0;

function canvasX(e: MouseEvent | PointerEvent, canvas: HTMLCanvasElement): number {
  // The drawing space is the canvas content box (core/store.ts W). Under a UI scale the
  // element is CSS-zoomed, so clientWidth is in *local* px while the pointer and the rect are
  // in screen px: convert with the zoomed width (rect includes the 1px border on each side).
  const rect = canvas.getBoundingClientRect();
  const scale = getUiScale();
  const border = (parseFloat(getComputedStyle(canvas).borderLeftWidth) || 0) * scale;
  const box = Math.max(1, rect.width - 2 * border);
  return (e.clientX - rect.left - border) * (S.W / box);
}

function canvasY(e: MouseEvent | PointerEvent, canvas: HTMLCanvasElement): number {
  // Same conversion as canvasX, on the other axis.
  const rect = canvas.getBoundingClientRect();
  const scale = getUiScale();
  const border = (parseFloat(getComputedStyle(canvas).borderTopWidth) || 0) * scale;
  const box = Math.max(1, rect.height - 2 * border);
  return (e.clientY - rect.top - border) * (S.H / box);
}

/**
 * The axis band under a canvas point, or null while a measurement OWNS the device.
 *
 * While a harmonic / PNM measurement is running the control rail is greyed out (`body.meas-mode`)
 * and the command layer refuses the SWP-owned commands behind it, so a canvas gesture has to obey
 * the same rule: without this a drag sent a command that could only be refused, and the refusal
 * arrives as an alert popup.
 */
function axisBandFor(x: number, y: number,
                     pr: { x: number; y: number; w: number; h: number }): Axis | null {
  // Two checks, because the client's own measurement flag only knows about a measurement IT started:
  // the device's reported mode covers the ones started elsewhere.
  if (measurementOwnsDevice()) return null;
  if (S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm')) return null;
  return axisBandAt(x, y, pr);
}

function axisBandCursor(band: Axis | null): string {
  return band === 'x' ? 'ew-resize' : band === 'y' ? 'ns-resize' : '';
}

type CanvasGesture = {
  owner: 'axis' | 'filter' | 'marker' | 'zoom' | 'tune' | 'jump';
  sdr: boolean;
  mode: string;
  demod: string;
  zoom: boolean;
  zoomEpoch: number;
  x0: number;
  clientX0: number;
  clientY0: number;
  moved: boolean;
  marquee: boolean;
  shift: boolean;
  marker?: S.MarkerState;
  markerBefore?: S.MarkerState;
  activeBefore?: number;
};
let canvasGesture: CanvasGesture | null = null;
let canvasPointerId: number | null = null;

function cancelZoomGesture(): void {
  if (cancelMarqueePreview()) requestRender();
}

function syncCanvasMarkerSelection(): void {
  document.querySelectorAll('.mkr-btn').forEach(button => {
    button.classList.toggle('active', button.classList.contains(`M${S.activeMkrId}`));
  });
  syncMarkerTrackingToggle();
  updateMarkersAllBtn();
}

function markerDiamondAt(x: number, y: number, canvas: HTMLCanvasElement): S.MarkerState | null {
  const powers = getDisplayPowers();
  if (!powers?.length) return null;
  const rect = canvas.getBoundingClientRect();
  const style = getComputedStyle(canvas);
  const sx = Math.max(1, rect.width - 2 * (parseFloat(style.borderLeftWidth) || 0) * getUiScale()) / S.W;
  const sy = Math.max(1, rect.height - 2 * (parseFloat(style.borderTopWidth) || 0) * getUiScale()) / S.H;
  let nearest: S.MarkerState | null = null;
  let distance = Infinity;
  for (const marker of S.markers) {
    if (!marker.enabled || marker.mode === 'OFF') continue;
    const idx = Math.min(marker.idx, powers.length - 1);
    const power = powers[idx];
    if (!Number.isFinite(power)) continue;
    const valley = idx > 0 && idx < powers.length - 1 && power <= powers[idx - 1] && power <= powers[idx + 1];
    const peak = idx > 0 && idx < powers.length - 1 && power >= powers[idx - 1] && power >= powers[idx + 1];
    const dx = (x - getX(idx, powers.length)) * sx;
    const dy = (y - (getY(power) + (valley && !peak ? 15 : -15))) * sy;
    const d = dx * dx + dy * dy;
    if (Math.abs(dx) <= 9 && Math.abs(dy) <= 9
        && (d < distance || (d === distance && marker.id === S.activeMkrId))) {
      nearest = marker;
      distance = d;
    }
  }
  return nearest;
}

function cancelSdrCanvasGesture(restore = true): void {
  const gesture = canvasGesture;
  const pointerId = canvasPointerId;
  canvasGesture = null;
  canvasPointerId = null;
  const canvas = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (pointerId !== null && canvas?.hasPointerCapture?.(pointerId)) canvas.releasePointerCapture(pointerId);
  if (canvas) canvas.style.cursor = '';
  cancelFilterDrag();
  setFilterHover(null);
  cancelSdrLocate();
  if (axisDragging()) resetAxisPreview();
  cancelZoomGesture();
  if (restore && gesture?.owner === 'tune' && gesture.moved) sdrListenHz.reset();
  if (restore && gesture?.marker && gesture.markerBefore) {
    Object.assign(gesture.marker, gesture.markerBefore);
    S.setActiveMkrId(gesture.activeBefore!);
    syncCanvasMarkerSelection();
  }
  S.setDragging(false);
  requestRender();
}

function canvasGestureCurrent(): boolean {
  const gesture = canvasGesture;
  if (!gesture) return false;
  if (gesture.mode !== currentGraphMode() || gesture.zoom !== isSpectrumZoomOn()
      || gesture.zoomEpoch !== zoomEnabled.epoch || (gesture.sdr && gesture.demod !== sdrDemod.get())) {
    cancelSdrCanvasGesture();
    return false;
  }
  return true;
}

export function bindCanvas() {
  const canvas = document.getElementById('spectrum') as HTMLCanvasElement;
  if (!canvas) return;

  canvas.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || canvasPointerId !== null) return;
    const active = document.activeElement;
    if (active instanceof HTMLInputElement && active.id === 'input-sdr-ifbw') active.blur();
    const pr = plotRectPub();
    const x = canvasX(e, canvas), y = canvasY(e, canvas);
    const mode = currentGraphMode();
    const sdr = mode === 'sdr';
    const zoom = isSpectrumZoomOn();
    const accept = (owner: CanvasGesture['owner']): CanvasGesture => {
      const gesture: CanvasGesture = {
        owner, sdr, mode, demod: sdrDemod.get(), zoom, zoomEpoch: zoomEnabled.epoch,
        x0: x, clientX0: e.clientX, clientY0: e.clientY,
        moved: false, marquee: false, shift: e.shiftKey,
      };
      canvasGesture = gesture;
      canvasPointerId = e.pointerId;
      setFilterHover(null);
      S.setDragging(true);
      canvas.setPointerCapture?.(e.pointerId);
      e.preventDefault();
      return gesture;
    };
    const band = axisBandFor(x, y, pr);
    if (band) {
      cancelSdrLocate();
      if (zoom) {
        resetViewForDeviceGesture();
        syncSpectrumZoomUi();
      }
      if (axisDragStart(band, x, y, pr)) {
        accept('axis');
        canvas.style.cursor = 'grabbing';
      }
      return;
    }
    if (x < pr.x || x > pr.x + pr.w || y < pr.y || y > pr.y + pr.h) return;
    const powers = getDisplayPowers();
    const hit = markerDiamondAt(x, y, canvas);
    if (e.ctrlKey || hit || (!sdr && !zoom && !e.shiftKey)) {
      cancelSdrLocate();
      const marker = e.ctrlKey ? S.markers.find(m => m.id === S.activeMkrId) : hit ?? S.markers.find(m => m.id === S.activeMkrId);
      if (!powers || !marker) return;
      const gesture = accept('marker');
      gesture.marker = marker;
      gesture.markerBefore = { ...marker };
      gesture.activeBefore = S.activeMkrId;
      S.setActiveMkrId(marker.id);
      marker.tracking = false;
      if (!hit || e.ctrlKey) placeMarkerFromX(x, powers);
      syncCanvasMarkerSelection();
      requestRender();
      return;
    }
    if (sdr) {
      const tolerance = 8 * S.W / Math.max(1, canvas.getBoundingClientRect().width);
      const edge = !zoom && filterEdgeAt(x, y, tolerance);
      if (edge && startFilterDrag(edge, e.pointerId)) {
        cancelSdrLocate();
        accept('filter');
        return;
      }
      // Freeze a locate candidate for a possible click; ownership is decided by zoom.
      beginSdrLocateClick(x, y, S.W / Math.max(1, canvas.getBoundingClientRect().width));
      accept(zoom ? 'zoom' : 'tune');
    } else if (powers) {
      cancelSdrLocate();
      accept(zoom ? 'zoom' : 'jump');
    }
  });

  window.addEventListener('pointermove', (e) => {
    if (e.pointerId !== canvasPointerId || !canvasGestureCurrent()) return;
    const gesture = canvasGesture!;
    const x = canvasX(e, canvas), y = canvasY(e, canvas);
    const dx = e.clientX - gesture.clientX0, dy = e.clientY - gesture.clientY0;
    if (Math.hypot(dx, dy) > 4) gesture.moved = true;
    if (gesture.owner === 'filter') {
      moveFilterDrag(x, e.pointerId);
    } else if (gesture.owner === 'axis') {
      axisDragMove(x, y);
      requestRender();
    } else if (gesture.owner === 'zoom') {
      if (gesture.marquee || isMarqueeDrag(dx, dy)) {
        gesture.marquee = true;
        cancelSdrLocate();
        if (updateMarqueePreview(gesture.x0, x)) requestRender();
      }
    } else if (gesture.owner === 'marker') {
      const powers = getDisplayPowers();
      if (powers && gesture.moved) {
        S.setActiveMkrId(gesture.marker!.id);
        placeMarkerFromX(x, powers);
      }
    } else if (gesture.owner === 'tune' && gesture.moved) {
      cancelSdrLocate();
      const f = spectrumFreqAtX(x);
      if (f != null) sdrListenHz.set(f);
      const pr = plotRectPub();
      const frac = (x - pr.x) / pr.w;
      const now = performance.now();
      if (now - sdrEdgeAt > 350 && (frac > 0.9 || frac < 0.1)) {
        sdrEdgeAt = now;
        sdrPanCentreBy(frac > 0.9 ? 0.2 : -0.2);
      }
      requestRender();
    }
  });

  window.addEventListener('pointerup', (e) => {
    if (e.pointerId !== canvasPointerId || !canvasGestureCurrent()) return;
    const gesture = canvasGesture!;
    const x = canvasX(e, canvas), y = canvasY(e, canvas);
    const pr = plotRectPub();
    const inside = x >= pr.x && x <= pr.x + pr.w && y >= pr.y && y <= pr.y + pr.h;
    let tune: number | null = null;
    let jump: number | null = null;
    let filter: number | null = null;
    if (gesture.owner === 'filter') {
      filter = endFilterDrag(e.pointerId);
    } else if (gesture.owner === 'axis') {
      axisDragEnd();
    } else if (gesture.owner === 'zoom' && gesture.marquee) {
      commitMarquee(gesture.x0, x, minSpanHz(S.freqArray));
      syncSpectrumZoomUi(true);
    } else if (gesture.owner === 'tune' || gesture.owner === 'zoom') {
      if (gesture.owner === 'tune' && gesture.moved) {
        tune = spectrumFreqAtX(x);
      } else if (inside && !gesture.moved) {
        const raw = spectrumFreqAtX(x);
        if (gesture.sdr) tune = raw === null ? null : commitSdrLocateClick(raw);
        else if (gesture.shift) jump = raw;
        else {
          const powers = getDisplayPowers();
          if (powers) placeMarkerFromX(x, powers);
        }
      }
    } else if (gesture.owner === 'marker' && gesture.moved) {
      const powers = getDisplayPowers();
      if (powers) {
        S.setActiveMkrId(gesture.marker!.id);
        placeMarkerFromX(x, powers);
      }
    } else if (gesture.owner === 'jump' && inside && !gesture.moved) {
      jump = spectrumFreqAtX(x);
    }
    if (gesture.owner === 'tune' && gesture.moved && tune === null) sdrListenHz.reset();
    cancelSdrCanvasGesture(false);
    if (filter !== null) selectSdrIfbw(filter);
    if (tune !== null) requestSdrTune(tune);
    if (jump !== null) listenAtFreq(jump);
  });

  // Hover feedback for the axis bands: without a cursor change the handles are invisible, and
  // the bands sit in the margins where nothing else ever happened before.
  canvas.addEventListener('mousemove', (e) => {
    if (canvasPointerId !== null || axisDragging() || filterDragging()) return;
    const x = canvasX(e, canvas), y = canvasY(e, canvas);
    const tolerance = 8 * S.W / Math.max(1, canvas.getBoundingClientRect().width);
    const band = axisBandFor(x, y, plotRectPub());
    const marker = markerDiamondAt(x, y, canvas);
    const edge = !isSpectrumZoomOn() && currentGraphMode() === 'sdr' ? filterEdgeAt(x, y, tolerance) : null;
    setFilterHover(!marker && !e.ctrlKey ? edge : null);
    if (currentGraphMode() === 'sdr' && !edge && !marker && !e.ctrlKey && !band) {
      updateSdrLocateHover(x, y, S.W / Math.max(1, canvas.getBoundingClientRect().width));
    } else cancelSdrLocate();
    const center = currentGraphMode() === 'sdr' && filterCenterAt(x, y, tolerance);
    const cursor = band ? axisBandCursor(band) : marker || e.ctrlKey ? 'crosshair'
      : edge ? 'ew-resize' : center && !isSpectrumZoomOn() ? 'grab' : '';
    if (canvas.style.cursor !== cursor) canvas.style.cursor = cursor;
  });
  // Cursor readout: publish the drawn bin under the pointer while the pointer is inside the plot.
  // The canvas then labels that point (see render/spectrum.ts). Only a change of bin repaints. The
  // readout cannot change faster than that, and one repaint redraws the whole trace and the tables.
  let cursorIdx: number | null = null;
  const publishCursor = (x: number, y: number) => {
    const pr = plotRectPub();
    const powers = getDisplayPowers();
    const inside = x >= pr.x && x <= pr.x + pr.w && y >= pr.y && y <= pr.y + pr.h;
    const idx = inside && powers && powers.length > 1 ? nearestIdxAtX(x, powers.length) : null;
    if (idx === cursorIdx) return;
    cursorIdx = idx;
    S.setPlotCursorIdx(idx);
    requestRender();
  };
  canvas.addEventListener('mousemove', (e) => {
    publishCursor(canvasX(e, canvas), canvasY(e, canvas));
  });
  canvas.addEventListener('mouseleave', () => publishCursor(-1e9, -1e9));
  canvas.addEventListener('mouseleave', () => {
    if (canvasPointerId === null) { setFilterHover(null); cancelSdrLocate(); }
  });

  // Wheel on an axis band zooms that axis (span on the frequency row, dB/div on the level
  // labels); inside the plot the wheel zooms the DISPLAY view while the toggle is On
  // (Ctrl/Cmd stays browser zoom), and stays page scrolling otherwise.
  canvas.addEventListener('wheel', (e) => {
    // Touchpad pinch arrives as Ctrl+wheel. Inside the plot the modifier already means
    // browser zoom; an axis band must obey the same rule — without this the pinch over the
    // frequency row accumulated a device window change and committed it (a retune the user
    // never asked for).
    if (e.ctrlKey || e.metaKey || canvasPointerId !== null) return;
    const pr = plotRectPub();
    const x = canvasX(e, canvas);
    const y = canvasY(e, canvas);
    const band = axisBandFor(x, y, pr);
    if (band) {
      cancelSdrLocate();
      if (isSpectrumZoomOn()) {
        resetViewForDeviceGesture();
        syncSpectrumZoomUi();
      }
      e.preventDefault();
      axisWheel(band, e.deltaY, x, y, pr);
      return;
    }
    if (isSpectrumZoomOn() && !e.ctrlKey && !e.metaKey
        && y >= pr.y && y <= pr.y + pr.h
        && spectrumWheelZoom(e.deltaY, x, minSpanHz(S.freqArray))) {
      e.preventDefault();
      cancelSdrLocate();
      syncSpectrumZoomUi();
      requestRender();
    }
  }, { passive: false });

  window.addEventListener('keydown', (e) => {
    if (e.key === 'Control' && canvasPointerId === null) {
      cancelSdrLocate();
      setFilterHover(null);
    }
    if (e.key === 'Escape' && canvasPointerId !== null) {
      cancelSdrCanvasGesture();
      canvas.style.cursor = '';
      e.preventDefault();
    }
  });
  window.addEventListener('pointercancel', (e) => {
    if (e.pointerId === canvasPointerId) cancelSdrCanvasGesture();
  });
  canvas.addEventListener('lostpointercapture', (e) => {
    if (e.pointerId === canvasPointerId) cancelSdrCanvasGesture();
  });
  let allowKeyRepeat = true;
  const stopKeyboardTune = () => {
    allowKeyRepeat = false;
    stopSdrTuneRepeat();
    cancelSdrCanvasGesture();
    canvas.style.cursor = '';
  };
  window.addEventListener('blur', stopKeyboardTune);
  document.addEventListener('visibilitychange', () => { if (document.hidden) stopKeyboardTune(); });

  // Keyboard / trackpad-only operation (no mouse required).
  document.addEventListener('keydown', (e) => {
    if (currentGraphMode() !== 'sdr' || e.defaultPrevented || e.altKey || e.metaKey || e.isComposing
        || (e.ctrlKey && e.shiftKey) || canvasPointerId !== null || document.hidden) return;
    const el = document.activeElement as HTMLElement | null;
    if (el?.closest('input,select,textarea,[contenteditable]:not([contenteditable="false"]),[role="dialog"],.keypad,#zoom-overview-box')) return;
    const arrow = e.key === 'ArrowLeft' || e.key === 'ArrowRight';
    if (el?.tagName === 'BUTTON' && !arrow) return;
    if ((e.ctrlKey || e.shiftKey) && !arrow) return;
    if (e.repeat && !allowKeyRepeat) return;
    if (!e.repeat) allowKeyRepeat = true;
    const step = currentSdrStepHz(e.ctrlKey ? 'fine' : e.shiftKey ? 'coarse' : 'normal');
    let handled = true;
    if (e.key === 'ArrowLeft') sdrTuneBy(-step);
    else if (e.key === 'ArrowRight') sdrTuneBy(step);
    else if (e.key === 'ArrowUp') sdrNudgeVolume(0.05);
    else if (e.key === 'ArrowDown') sdrNudgeVolume(-0.05);
    else if (e.key === 'PageUp') sdrCycleIfbw(1);
    else if (e.key === 'PageDown') sdrCycleIfbw(-1);
    else if (e.key === 'm' || e.key === 'M') sdrCycleDemod();
    else if (e.key === ' ') toggleSdrAudio();
    else handled = false;
    if (handled) e.preventDefault();
  });
}

// ── SDR keyboard helpers ──
// Filter keyboard choices use the current mode's presets.

function sdrTuneBy(dHz: number) {
  const center = sdrCenterHz.get();
  const span = sdrSpanHz.get();
  if (!(center > 0) || !(span > 0)) return;
  const f = Math.max(center - span / 2,
    Math.min(center + span / 2, (sdrListenHz.get() || center) + dHz));
  requestSdrTune(f);
}

function sdrCycleIfbw(dir: number) {
  const spec = filterSpec(sdrDemod.get());
  if (!spec.editable) return;
  const cur = sdrIfbw.get();
  const choices = spec.presets;
  const next = dir > 0 ? choices.find(v => v > cur) : [...choices].reverse().find(v => v < cur);
  if (next != null) selectSdrIfbw(next);
}

function sdrCycleDemod() {
  // The cycle is the registry's list (analog then digital), not a copy of it: a mode added to the
  // DSP appears here without another edit.
  const modes = sdrModeIds();
  if (modes.length === 0) return;                 // manifest not loaded yet: keep the current mode
  const current = sdrDemod.get();
  const index = modes.indexOf(current);
  selectSdrDemod(modes[(index + 1) % modes.length]);
}

function sdrNudgeVolume(dv: number) {
  // `|| 0.8` here had the same defect as the slider's: at 0 the nudge started from 0.8 again.
  const from = Number.isFinite(sdrVolume.get()) ? sdrVolume.get() : 0.8;
  const next = Math.max(0, Math.min(2, from + dv));
  sdrVolume.set(next);
  const inp = document.getElementById('input-sdr-volume') as HTMLInputElement | null;
  if (inp) inp.value = String(next);
  applySdrDemod();
}
import { plotRect as plotRectPub, nearestIdxAtX, getX, getY } from '../render/plot';
import { exitMeasMode as exitMeasModePub } from './measure';
import { smoothBins } from './displayState';
import { peakListVisible } from './measurePrefs';
import { spanStepAuto } from './swpState';

// ── Re-exports for the rest of the app ──
// The panel modules own these actions; the previous public surface (everything imported
// from ui/controls) is kept so no caller had to change (report finding P1-5).
export {
  applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetSpanStepAuto,
  stepSwpSpan, syncFrequencyEditorStatus, syncSwpSpanStep, updateCustomSpanStep,
} from './panels/frequency';
export { applyPoints, applyRBW, applyVBW, setFrameDetector, setPointsStrategy, setSpurMode, setTraceAlign, setWindow } from './panels/resolution';
export {
  applyRta, clearRtaAccum, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins, setRtaDensSource, setRtaHalflife,
} from './panels/rta';
export {
  activeMarkerPeak, activeMarkerValley, autoTrackMarker, markerToCenter, placeMarkerFromX,
  selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll,
  updateMarkersAllBtn,
} from './panels/markers';
export {
  adjustRefLevel, refStepDbm, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel,
  setScale, syncRefClockStatus, syncScaleButtons, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
export {
  resetWf, setSweepSpeed, syncSweepInput, toggleWaterfall, toggleWfPause,
} from './panels/waterfall';
export { closeGnssDetail, fillGnssDetail } from './panels/gnss';
export { syncToggleIcons, syncToggleTexts, toggleAllGroups, toggleGroup } from './panels/groups';
export { commitUnitField } from './panels/commit';
export { currentGraphMode };
