// Control commands + data-action binding + panel collapse + marker ops + canvas interaction
import * as S from '../core/store';
import { send } from '../core/wsSend';

import { resetSdrAutoRef } from '../core/sdrAutoRef';
import { sdrCenterHz, sdrDecimate, sdrAudioOn, sdrDeemph, sdrDemod, sdrIfbw, sdrListenHz, sdrRefAuto, sdrSpanHz, estimatedCaptureSpanHz, renderSdrState, resetSdrState } from './sdrState';
import { centerHz, swpCenterHz } from './freqState';
import { updateInfoBar } from '../render/infobar';
import { requestRender } from '../render/redraw';
import { getDisplayPowers } from '../dsp/peaks';

import { normRefWindow, setNormRefWinUser, smoothRefWindow, buildReferenceTablePub } from './normPub';
import { switchTraceTab, toggleFreeze, setTraceMode, clearRtaTrace, setTraceAverage, exportActiveTraceCsv, exportPeakListCsv } from './traceOps';
import { exportSpectrumPng } from './exportImage';
import { normalizeActiveTrace, resetActiveTraceNormalize } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
import { togglePeakList, peakThrManual, peakThrAuto } from '../render/peaklist';
import { measToggle, measTab, applyMeasUI, setMeasButtons } from './measure';
import { measureAmp, clearAmp } from '../meas/amplitude';
import { measureChannel, clearChannel } from '../meas/channel';
import { measHarmApply, autoHarmSpan } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';

import { t } from '../core/i18n';
import { openRefClockDetail, closeRefClockDetail } from '../core/refclock';
import { prepareSdrAudioTransition, setSdrAudioEnabled } from '../audio/sdrAudio';
import { resetLimits } from './limits';

// Panel modules (report finding P1-5). controls.ts keeps the wiring (event binding, canvas
// interaction, mode/preset orchestration) and imports the actions it dispatches; the
// re-exports below keep the previous public surface for the rest of the app.
import { applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetSpanStepAuto, stepSwpSpan, syncSwpSpanStep, updateCustomSpanStep } from './panels/frequency';
import { applyPoints, applyRBW, applyVBW, setSpurMode, setWindow } from './panels/resolution';
import { applyRta, clearRtaAccum, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins } from './panels/rta';
import { activeMarkerNextPeakLeft, activeMarkerNextPeakRight, activeMarkerNextValleyLeft, activeMarkerNextValleyRight, activeMarkerPeak, activeMarkerValley, markerToCenter, placeMarkerFromX, selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll } from './panels/markers';
import {
  adjustRefLevel, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel, setScale,
  syncSdrRefUI, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
import { resetWf, setSweepSpeed, syncSweepInput, toggleWaterfall, toggleWfPause } from './panels/waterfall';
import { closeGnssDetail, fillGnssDetail } from './panels/gnss';
import { toggleAllGroups, toggleGroup } from './panels/groups';
import { commitUnitField } from './panels/commit';

// ── Frequency linking ──

export function connectDevice() { send({ cmd: 'CONNECT' }); }

// ── Marker operations ──
// Graph-mode + display-reference requests are owned by ui/graphMode.ts and ui/displayRef.ts.
// They apply the four disciplines (id-matched ack, supersede, timeout notice, visible
// divergence); this module only translates them to the DOM.
import { currentGraphMode, graphModeDiverges, isGraphMode, pendingGraphMode, requestGraphMode, confirmGraphMode, resetGraphMode, setGraphModeTimeoutHandler } from './graphMode';
import { setDisplayRef, setDisplayRefTimeoutHandler } from './displayRef';

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
  flashHint('ref-hint', t('ref_switch_timeout'));
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
  syncModeButtons();
  // Sweep-to-SDR handoff: entering SDR from the swept view demodulates the frequency
  // the user located (active marker), or the current centre if no marker is set.
  if (target === 'sdr') {
    // Respect an explicit handoff (Shift+click / peak row); otherwise use the active
    // marker, else the current centre.
    if (!sdrCenterHz.pending()) {
      const m = S.markers.find(x => x.enabled && x.freq != null);
      // Prefer the last centre confirmed by a SWP-family STATUS: centerHz.get() is refreshed
      // from every STATUS (including SDR ones), so it can still hold the value from before
      // a preset/re-tune when this runs.
      const base = swpCenterHz.get() > 0 ? swpCenterHz.get() : centerHz.get();
      sdrCenterHz.set((m && m.freq) ? m.freq : base);
    }
    deferSdrAudioPreference();
  } else {
    sdrCenterHz.reset();
    if (sdrAudioHandoffTimer !== null) {
      window.clearTimeout(sdrAudioHandoffTimer);
      sdrAudioHandoffTimer = null;
    }
    setSdrAudioEnabled(false);
  }
  send({ cmd: 'SET_MODE', mode: target });
}

/** Drop a pending request (socket loss / command error): the UI follows the backend again. */
export function releaseGraphModePending() {
  resetGraphMode();
  syncModeButtons();
}

export function syncGraphModeStatus(mode: string) {
  if (!isGraphMode(mode)) return;
  // Discipline 1: an older reply (a STATUS still on another mode) is not our answer.
  const changed = confirmGraphMode(mode);
  syncModeButtons();
  if (!changed) return;
  const isRtaLike = mode === 'rta' || mode === 'sdr';
  const isSdr = mode === 'sdr';
  S.setRtaMode(isRtaLike);
  S.setViewMode(isRtaLike ? 'rta' : 'std');
  S.setSdrMode(isSdr);
  if (isSdr) {
    resetSdrAutoRef();
    deferSdrAudioPreference();
    // Re-issue the reference to the device on entry. It re-applies the IQS reference level
    // and clears a stale acquisition, so the first automatic scale has a sane frame to work
    // with (the reported "Preset -> SDR spectrum overflows the canvas"). Auto is the client's
    // preference here, not the swept one.
    if (sdrRefAuto.get()) {
      send({ cmd: 'SET_REF', mode: 'auto', range_db: S.totalDivs * S.dbPerDiv });
    }
  } else {
    setSdrAudioEnabled(false);
    sdrAudioOn.set(false);
    syncSdrAudioButton();
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
    'select-detector',
    'input-points', 'btn-points',
  ];
  rtaDisable.forEach((id) => {
    const element = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
    if (element) element.disabled = isRtaLike;
  });
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
    'select-spur', 'btn-gapfill', 'trigger-panel',
  ].forEach((id) => setDisabled(id, isSdr));
  const resetButton = document.querySelector(
    '[data-action="reset-norm"]') as HTMLButtonElement | null;
  if (resetButton) resetButton.disabled = isRtaLike;
  const rtaFrequency = document.getElementById('rta-freq-settings');
  const swpFrequency = document.getElementById('swp-freq-settings');
  const sdrSettings = document.getElementById('sdr-settings');
  if (rtaFrequency) rtaFrequency.style.display = mode === 'rta' ? '' : 'none';
  if (swpFrequency) swpFrequency.style.display = mode === 'std' ? '' : 'none';
  if (sdrSettings) sdrSettings.style.display = isSdr ? '' : 'none';
  if (isRtaLike) S.resetWaterfall();
  if (isSdr && sdrCenterHz.pending() && sdrCenterHz.get() > 0) {
    const f = sdrCenterHz.get();
    const inFm = f >= 87.5e6 && f <= 108e6;
    const inAir = f >= 118e6 && f <= 137e6;
    sdrDecimate.set(16);
    sdrSpanHz.set(estimatedCaptureSpanHz(16)); // estimate until the device reports the real span
    sdrListenHz.set(f);
    renderSdrState();
    sdrDemod.set(inFm ? 'wfm' : 'am');
    sdrIfbw.set(inFm ? 180000 : (inAir ? 25000 : 12000));
    renderSdrState();
    send({ cmd: 'SET_SDR', center: f, decimate: 16 });
    send({ cmd: 'SET_SDR_TUNE', listen: f });
    applySdrDemod();
  }
  requestRender();
}

// Density persistence/grain restored on page load + RTA entry (independent memory keys)
// ── SDR mode controls ──

function sdrNumber(id: string, fallback: number): number {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  const v = el ? parseFloat(el.value) : NaN;
  return isFinite(v) ? v : fallback;
}

function sdrAgcOn(): boolean {
  const el = document.getElementById('btn-sdr-agc');
  return el ? el.classList.contains('active') : true;
}

export function applySdr() {
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
  send({ cmd: 'SET_SDR_TUNE', listen: center });
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

// Changing only the capture bandwidth keeps the listen frequency unchanged.
export function applySdrBw() {
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
  sdrListenHz.set(f);
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR_TUNE', listen: f });
}

export function applySdrDemod() {
  const mode = sdrDemod.get();
  const ifbw = sdrIfbw.get();
  const deemph = sdrDeemph.get();
  const volume = sdrNumber('input-sdr-volume', 0.8);
  const squelch = sdrNumber('input-sdr-squelch', -110);
  // Only a demod-mode / IF-bandwidth / de-emphasis change rebuilds the chain and needs the
  // reset handshake. Volume/squelch/AGC are applied live, so muting them would just add a
  // gap. "Changed" is the slot's own pending state now, not a separate copy of the last
  // value sent (those caches were one more thing that could disagree with the truth).
  if (sdrDemod.pending() || sdrIfbw.pending() || sdrDeemph.pending()) {
    prepareSdrAudioTransition();
  }
  send({ cmd: 'SET_SDR_DEMOD', mode, ifbw, volume, squelch, agc: sdrAgcOn(),
         deemph_us: deemph });
}

export function toggleSdrAgc(el: HTMLElement) {
  const on = !el.classList.contains('active');
  el.classList.toggle('active', on);
  el.textContent = on ? t('on') : t('off');
  // AGC is applied live on the backend; no chain rebuild, so no mute/reset.
  send({ cmd: 'SET_SDR_DEMOD', agc: on });
}

// Band presets (centre, capture bandwidth, demod, IF bandwidth)
const SDR_BANDS: Record<string, { center: number; decimate: number; demod: string; ifbw: number }> = {
  fm: { center: 98e6, decimate: 2, demod: 'wfm', ifbw: 180000 },
  air: { center: 127.5e6, decimate: 16, demod: 'am', ifbw: 25000 },
  vhf: { center: 145e6, decimate: 16, demod: 'fm', ifbw: 12000 },
  uhf: { center: 435e6, decimate: 16, demod: 'fm', ifbw: 12000 },
};

export function applySdrBand(name: string) {
  const b = SDR_BANDS[name];
  if (!b) return;
  prepareSdrAudioTransition();
  sdrCenterHz.set(b.center);
  sdrDecimate.set(b.decimate);
  sdrSpanHz.set(estimatedCaptureSpanHz(b.decimate)); // estimate until the device reports the real span
  sdrDemod.set(b.demod);
  sdrIfbw.set(b.ifbw);
  sdrListenHz.set(b.center);
  renderSdrState();
  send({ cmd: 'SET_SDR', center: b.center, decimate: b.decimate });
  send({ cmd: 'SET_SDR_TUNE', listen: b.center });
  applySdrDemod();
}

export function listenAtFreq(hz: number) {
  if (!isFinite(hz) || hz <= 0) return;
  if (currentGraphMode() === 'sdr') {
    sdrListenHz.set(hz);
    renderSdrState();
    prepareSdrAudioTransition();
    send({ cmd: 'SET_SDR_TUNE', listen: hz });
    requestRender();
    return;
  }
  // From the swept view: hand this frequency to SDR for demodulation.
  sdrCenterHz.set(hz);
  setGraphMode('sdr');
}

function syncSdrButtons() {
  const mode = sdrDemod.get();
  document.querySelectorAll('[data-sdr-demod]').forEach((el) => {
    el.classList.toggle('active', (el as HTMLElement).dataset.sdrDemod === mode);
  });
  const ibw = Math.round(sdrIfbw.get());
  document.querySelectorAll('[data-sdr-ifbw]').forEach((el) => {
    el.classList.toggle('active', Number((el as HTMLElement).dataset.sdrIfbw) === ibw);
  });
  const dv = sdrDeemph.get();
  document.querySelectorAll('[data-sdr-deemph]').forEach((el) => {
    el.classList.toggle('active', Number((el as HTMLElement).dataset.sdrDeemph) === dv);
  });
}

// ── SDR audio enable + amplitude reference ──

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
  setSdrAudioEnabled(on);
  syncSdrAudioButton();
}

export function toggleSdrAudio() {
  const on = !sdrAudioOn.get();
  sdrAudioOn.set(on); // the slot persists it (single writer)
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
  if (!sdr) return;
  const a = sdr.actual || {};
  // Confirm the tuning group from the backend, then render it from the slots (the only
  // writer of those controls). The demod group still uses the DOM for now.
  sdrCenterHz.confirm(Number(sdr.center) || 0);
  sdrDecimate.confirm(Number(sdr.decimate) || 32);
  if (a.start != null && a.stop != null) sdrSpanHz.confirm(Number(a.stop) - Number(a.start));
  sdrListenHz.confirm(Number(sdr.listen) || 0);
  sdrDemod.confirm(String(sdr.demod || 'am'));
  sdrIfbw.confirm(Number(sdr.if_bw) || 6000);
  // The requested de-emphasis (-1 = per-mode default). Without this confirm the user's
  // choice stayed a pending intent and fell back to Auto when the slot TTL expired.
  if (sdr.deemph_us != null) sdrDeemph.confirm(Number(sdr.deemph_us));
  renderSdrState();
  sdrSet('input-sdr-volume', String(sdr.volume));
  sdrSet('input-sdr-squelch', String(Math.round(Number(sdr.squelch))));
  const agc = document.getElementById('btn-sdr-agc');
  if (agc) {
    agc.textContent = sdr.agc ? t('on') : t('off');
    agc.classList.toggle('active', !!sdr.agc);
  }
  const lvl = document.getElementById('cur-sdr-level');
  if (lvl) lvl.textContent = Number.isFinite(sdr.level_dbfs) ? sdr.level_dbfs.toFixed(1) + ' dBFS' : '';
  syncSdrButtons();
  syncSdrAudioButton();
  syncSdrRefUI();
}
// 仅 ×N(6)/Manual(7) 需要输入框+Set 按钮; 其余固定档隐藏
// Turn all markers on/off at once (toggle)
// Preset
export function presetAll() {
  exitMeasModePub();
  S.setMeasOn(false);
  const b = document.getElementById('btn-meas-onoff');
  if (b) b.textContent = t('off');
  setMeasButtons(false);
  setDisplayRef('preset', 0);
  S.setDisplayOffset(0);
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of) of.value = '0';
  S.traces.forEach((t, i) => { t.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; t.reference = null; t.isNormalized = false; t.avgSum = null; t.avgCount = 0; });
  S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; });
  S.setM3dB(null); S.setAmpRes(null); S.setHarm(null); S.setPnmData(null);
  S.setPeakListOn(false); S.setPeakMarks(null);
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
    clearRtaAccum();
  }
  // Preset must also clear the browser-side records, otherwise a reload restores the old
  // mode / audio / ref / RTA-fade / limit-line state instead of the power-on defaults.
  try {
    ['web-sa-mode', 'web-sa-sdr-audio', 'web-sa-sdr-ref-auto',
     'rta-fade', 'rta-bins'].forEach((k) => localStorage.removeItem(k));
  } catch { /* ignore */ }
  resetLimits();
  S.resetWaterfall();
  S.setWfPaused(false);
  S.setSmoothBins(1);
  S.setSpanStepAuto(true);
  setSdrAudioEnabled(false);
  sdrAudioOn.set(false);
  sdrRefAuto.set(true);

  // Every pending SDR intent (including a hand-off centre) is dropped by one call - the old
  // code cleared the fields by hand and missed one, so a Preset could reapply the previous
  // frequency.
  resetSdrState();
  renderSdrState();
  resetSdrAutoRef();
  send({ cmd: 'SET_PRESET' });
  updateInfoBar(); applyMeasUI(); requestRender();
}

// Current frontend time (shown when not locked)
// GNSS detail popover: fill + show/close
// Sync all toggle button texts when the language changes
// ── Panel collapse ──
// ── data-action binding ──
export function bindActions() {
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
    'set-refclock': (el) => setRefClock((el as HTMLSelectElement).value),
    'toggle-refclkout': () => toggleRefClkOut(),
    'set-amp': () => setAmp(),
    'set-trace-mode': (el) => setTraceMode((el as HTMLSelectElement).value),
    'set-trace-avg': (el) => setTraceAverage(parseInt((el as HTMLSelectElement).value) || 0),
    'export-csv': () => exportActiveTraceCsv(),
    'export-png': () => exportSpectrumPng(),
    'export-peaks-csv': () => exportPeakListCsv(),
    'set-smooth': (el) => { S.setSmoothBins(parseInt((el as HTMLSelectElement).value) || 1); requestRender(); },
    'set-norm-refwin': (el) => {
      const v = parseInt((el as HTMLSelectElement).value) || 0;
      setNormRefWinUser(v);
      const t = S.traces[S.activeTraceIdx];
      if (t.reference && t.isNormalized && t.powers) {
        const ref = buildReferenceTablePub(t.powers);
        t.reference = smoothRefWindow(ref, normRefWindow());
      }
      requestRender();
    },
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
    'set-sdr-demod': () => applySdrDemod(),
    'toggle-sdr-agc': (el) => toggleSdrAgc(el),
    'toggle-sdr-audio': () => toggleSdrAudio(),
    'rta-span-down': () => rtaSpanStep(1),
    'rta-span-up': () => rtaSpanStep(-1),
    'rta-span-full': () => rtaSpanFull(),
    'set-rta-fade': (el) => { S.setRtaFade(parseFloat((el as HTMLSelectElement).value) || 0.98); try { localStorage.setItem('rta-fade', (el as HTMLSelectElement).value); } catch {} },
    'set-rta-bins': (el) => { setRtaBins(parseInt((el as HTMLSelectElement).value) || 128); },
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
  });
  document.addEventListener('websa:unit-commit', (event) => {
    const detail = (event as CustomEvent<{ field: string; commit: boolean }>).detail;
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
  ['input-sdr-volume', 'input-sdr-squelch'].forEach((id) => {
    const el = document.getElementById(id) as HTMLInputElement | null;
    if (el) el.addEventListener('change', () => applySdrDemod());
  });
  document.querySelectorAll('[data-sdr-demod]').forEach((el) => {
    el.addEventListener('click', () => {
      sdrDemod.set((el as HTMLElement).dataset.sdrDemod || 'am');
      renderSdrState();
      applySdrDemod();
    });
  });
  document.querySelectorAll('[data-sdr-ifbw]').forEach((el) => {
    el.addEventListener('click', () => {
      sdrIfbw.set(Number((el as HTMLElement).dataset.sdrIfbw) || 6000);
      renderSdrState();
      applySdrDemod();
    });
  });
  document.querySelectorAll('[data-sdr-deemph]').forEach((el) => {
    el.addEventListener('click', () => {
      sdrDeemph.set(Number((el as HTMLElement).dataset.sdrDeemph ?? -1));
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
  syncSdrRefUI();
  const sdrCenter = document.getElementById('input-sdr-center') as HTMLInputElement | null;
  if (sdrCenter) sdrCenter.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdr();
  });
  const sdrListen = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (sdrListen) sdrListen.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdrTune();
  });

  // Special bindings: trace tab / meas tab / marker select / scale / peakthr input
  document.querySelectorAll('[data-trace-tab]').forEach(el => {
    el.addEventListener('click', () => switchTraceTab(parseInt((el as HTMLElement).dataset.traceTab || '0')));
  });
  document.querySelectorAll('[data-meas-tab]').forEach(el => {
    el.addEventListener('click', () => measTab((el as HTMLElement).dataset.measTab || 'amp'));
  });

  const wfBtn = document.getElementById('btn-waterfall');
  if (wfBtn) wfBtn.addEventListener('click', () => toggleWaterfall());
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
  const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  if (sm) { sm.addEventListener('change', () => syncSweepInput()); syncSweepInput(); }
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
    input?.addEventListener('input', () => { input.dataset.edited = '1'; });
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
let sdrDown = false;
let sdrMoved = false;
let sdrX0 = 0;
let sdrEdgeAt = 0;

function canvasX(e: MouseEvent, canvas: HTMLCanvasElement): number {
  const rect = canvas.getBoundingClientRect();
  return (e.clientX - rect.left) * (S.W / rect.width);
}

function xToFreqHz(x: number): number | null {
  const f = S.freqArray;
  if (!f || f.length < 2) return null;
  const pr = plotRectPub();
  const t = (x - pr.x) / pr.w;
  if (t < 0 || t > 1) return null;
  const idx = t * (f.length - 1);
  const i0 = Math.max(0, Math.min(f.length - 2, Math.floor(idx)));
  const fr = idx - i0;
  return f[i0] * (1 - fr) + f[i0 + 1] * fr;
}

export function bindCanvas() {
  const canvas = document.getElementById('spectrum') as HTMLCanvasElement;
  if (!canvas) return;

  canvas.addEventListener('mousedown', (e) => {
    const pr = plotRectPub();
    const x = canvasX(e, canvas);
    if (currentGraphMode() === 'sdr') {
      if (x < pr.x || x > pr.x + pr.w) return;
      sdrDown = true; sdrMoved = false; sdrX0 = x;
      S.setDragging(true);
      return;
    }
    const p = getDisplayPowers();
    if (!p) return;
    if (x < pr.x || x > pr.x + pr.w) return;
    if (e.shiftKey) {
      // Shift+click: jump straight to SDR demodulation at this frequency.
      const f = xToFreqHz(x);
      if (f != null) listenAtFreq(f);
      return;
    }
    S.setDragging(true);
    placeMarkerFromX(x, p);
  });

  window.addEventListener('mousemove', (e) => {
    if (!S.dragging) return;
    const x = canvasX(e, canvas);
    if (sdrDown) {
      if (Math.abs(x - sdrX0) > 4) sdrMoved = true;
      if (sdrMoved) {
        // Preview the marker locally; the tune itself is committed on release.
        const f = xToFreqHz(x);
        if (f != null) sdrListenHz.set(f);
        // Edge push: dragging past the sides shifts the capture window so the user can
        // walk through adjacent frequency ranges (standard SDR panning). Throttled
        // because it retunes the device.
        const pr = plotRectPub();
        const frac = (x - pr.x) / pr.w;
        const now = performance.now();
        if (now - sdrEdgeAt > 350) {
          if (frac > 0.9) {
            sdrEdgeAt = now;
            sdrCenterHz.set(sdrCenterHz.get() + sdrSpanHz.get() * 0.2);
            sdrX0 += pr.w * 0.2;
            send({ cmd: 'SET_SDR', center: sdrCenterHz, decimate: sdrDecimate });
          } else if (frac < 0.1) {
            sdrEdgeAt = now;
            sdrCenterHz.set(sdrCenterHz.get() - sdrSpanHz.get() * 0.2);
            sdrX0 -= pr.w * 0.2;
            send({ cmd: 'SET_SDR', center: sdrCenterHz, decimate: sdrDecimate });
          }
        }
      }
      return;
    }
    const p = getDisplayPowers();
    if (!p) return;
    placeMarkerFromX(x, p);
  });

  window.addEventListener('mouseup', (e) => {
    if (sdrDown) {
      // Commit the tune once, on release (click or drag).
      const raw = xToFreqHz(canvasX(e, canvas));
      const f = raw;
      if (f != null) {
        sdrListenHz.set(f);
        send({ cmd: 'SET_SDR_TUNE', listen: f });
        requestRender();
      }
      sdrDown = false;
      sdrMoved = false;
      S.setDragging(false);
      return;
    }
    S.setDragging(false);
  });

  // Keyboard / trackpad-only operation (no mouse required).
  document.addEventListener('keydown', (e) => {
    if (currentGraphMode() !== 'sdr') return;
    const el = document.activeElement as HTMLElement | null;
    const tag = el?.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
    const mult = e.shiftKey ? 100 : (e.altKey ? 10 : 1);
    let handled = true;
    if (e.key === 'ArrowLeft') sdrTuneBy(-1000 * mult);
    else if (e.key === 'ArrowRight') sdrTuneBy(1000 * mult);
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
const SDR_IFBW = [500, 2400, 3000, 6000, 12000, 25000, 50000, 100000, 180000];
const SDR_MODES = ['am', 'fm', 'nfm', 'wfm', 'usb', 'lsb', 'cw'];

function sdrTuneBy(dHz: number) {
  const center = sdrCenterHz.get();
  const span = sdrSpanHz.get();
  if (!(center > 0) || !(span > 0)) return;
  const f = Math.max(center - span / 2,
    Math.min(center + span / 2, (sdrListenHz.get() || center) + dHz));
  sdrListenHz.set(f);
  renderSdrState();
  send({ cmd: 'SET_SDR_TUNE', listen: f });
  requestRender();
}

function sdrCycleIfbw(dir: number) {
  const cur = sdrIfbw.get();
  let idx = SDR_IFBW.findIndex(v => v >= cur);
  if (idx < 0) idx = SDR_IFBW.length - 1;
  else if (SDR_IFBW[idx] > cur) idx = Math.max(0, idx - 1);
  const ni = Math.max(0, Math.min(SDR_IFBW.length - 1, idx + dir));
  sdrIfbw.set(SDR_IFBW[ni]);
  renderSdrState();
  applySdrDemod();
}

function sdrCycleDemod() {
  const ni = (SDR_MODES.indexOf(sdrDemod.get()) + 1) % SDR_MODES.length;
  sdrDemod.set(SDR_MODES[ni]);
  renderSdrState();
  applySdrDemod();
}

function sdrNudgeVolume(dv: number) {
  const inp = document.getElementById('input-sdr-volume') as HTMLInputElement | null;
  if (!inp) return;
  inp.value = String(Math.max(0, Math.min(2, (parseFloat(inp.value) || 0.8) + dv)));
  applySdrDemod();
}
import { plotRect as plotRectPub } from '../render/plot';
import { exitMeasMode as exitMeasModePub } from './measure';

// ── Re-exports for the rest of the app ──
// The panel modules own these actions; the previous public surface (everything imported
// from ui/controls) is kept so no caller had to change (report finding P1-5).
export {
  applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetSpanStepAuto,
  stepSwpSpan, syncFrequencyEditorStatus, syncSwpSpanStep, updateCustomSpanStep,
} from './panels/frequency';
export { applyPoints, applyRBW, applyVBW, setSpurMode, setWindow } from './panels/resolution';
export {
  applyRta, clearRtaAccum, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins,
} from './panels/rta';
export {
  activeMarkerPeak, activeMarkerValley, autoTrackMarker, markerToCenter, placeMarkerFromX,
  selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll,
  updateMarkersAllBtn,
} from './panels/markers';
export {
  adjustRefLevel, refStepDbm, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel,
  setScale, syncRefClkOut, syncScaleButtons, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
export {
  resetWf, setSweepSpeed, syncSweepInput, toggleWaterfall, toggleWfPause,
} from './panels/waterfall';
export { closeGnssDetail, fillGnssDetail } from './panels/gnss';
export { syncToggleIcons, syncToggleTexts, toggleAllGroups, toggleGroup } from './panels/groups';
export { commitUnitField } from './panels/commit';
export { currentGraphMode };
