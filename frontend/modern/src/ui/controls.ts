// Control commands + data-action binding + panel collapse + marker ops + canvas interaction
import * as S from '../core/store';
import { send } from '../core/wsSend';
import { updateFreqUIInputs } from '../core/ws';
import { updateInfoBar } from '../render/infobar';
import { renderAll } from '../render/spectrum';
import { getDisplayPowers, nextExtreme, setMarkerIdx, getTraceDisplay } from '../dsp/peaks';
import { markerFreqHz } from '../core/markerCommon';
import { parseFreqUnit, toUnit } from '../core/units';
import {
  niceSpanStep,
  normalizeCenterSpan,
  normalizeStartStop,
  steppedRefLevel,
  steppedSpan,
} from '../core/frequency';
import { setSmoothBins } from '../core/store';
import { normRefWindow, setNormRefWinUser, smoothRefWindow, buildReferenceTablePub } from './normPub';
import { switchTraceTab, toggleFreeze, setTraceMode, clearRtaTrace, setTraceAverage, exportActiveTraceCsv, exportPeakListCsv } from './traceOps';
import { exportSpectrumPng } from './exportImage';
import { normalizeActiveTrace, resetActiveTraceNormalize, updateNormalizeStatusUI } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
import { togglePeakList, peakThrManual, peakThrAuto } from '../render/peaklist';
import { measToggle, measTab, applyMeasUI, setMeasButtons } from './measure';
import { measureAmp, clearAmp } from '../meas/amplitude';
import { measureChannel, clearChannel } from '../meas/channel';
import { measHarmApply, autoHarmSpan } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';
import { canvasColors, getTheme } from '../core/theme';
import { t } from '../core/i18n';
import { assignMarkerToBestPeak, toggleMarkerTracking } from '../dsp/markerTracking';
import { openRefClockDetail, closeRefClockDetail } from '../core/refclock';
import { setSdrAudioEnabled } from '../audio/sdrAudio';

// ── Frequency linking ──
function frequencyEditor(id: 'swp-freq-settings' | 'rta-freq-settings'): HTMLElement | null {
  return document.getElementById(id);
}

function markFrequencyDirty(id: 'swp-freq-settings' | 'rta-freq-settings') {
  const editor = frequencyEditor(id);
  if (editor) editor.dataset.dirty = '1';
}

function beginFrequencyCommit(id: 'swp-freq-settings' | 'rta-freq-settings') {
  const editor = frequencyEditor(id);
  if (editor) {
    editor.dataset.pending = '1';
    editor.dataset.pendingVersion = String(S.configVersion + 1);
    editor.dataset.pendingAt = String(Date.now());
  }
}

function clearFrequencyEditor(editor: HTMLElement) {
  delete editor.dataset.pending;
  delete editor.dataset.pendingVersion;
  delete editor.dataset.pendingAt;
  delete editor.dataset.dirty;
  editor.querySelectorAll('input').forEach(input => delete (input as HTMLElement).dataset.edited);
}

export function syncFrequencyEditorStatus(responseTo?: string, configVersion = 0) {
  const scalarInput = responseTo === 'SET_RBW'
    ? 'input-rbw'
    : responseTo === 'SET_VBW' ? 'input-vbw' : responseTo === 'SET_PNM' ? 'input-pnm' : null;
  if (scalarInput) {
    const input = document.getElementById(scalarInput);
    if (input) delete input.dataset.edited;
  }
  const id = responseTo === 'SET_FREQ'
    ? 'swp-freq-settings'
    : responseTo === 'SET_RTA' ? 'rta-freq-settings' : null;
  if (id) {
    const editor = frequencyEditor(id);
    if (editor) clearFrequencyEditor(editor);
    return;
  }
  for (const editorId of ['swp-freq-settings', 'rta-freq-settings'] as const) {
    const editor = frequencyEditor(editorId);
    if (!editor?.dataset.pending) continue;
    const expected = Number(editor.dataset.pendingVersion || Infinity);
    const pendingAt = Number(editor.dataset.pendingAt || Date.now());
    if (configVersion >= expected && Date.now() - pendingAt >= 2500) {
      clearFrequencyEditor(editor);
    }
  }
}

export function commitUnitField(field: string, commit = true) {
  if (field === 'span') syncSwpSpanStep();
  if (!commit) return;
  if (field === 'center' || field === 'span') {
    applyCenterSpan();
  } else if (field === 'start' || field === 'stop') {
    applyStartStop();
  } else if (field === 'rta_center') {
    applyRta();
  } else if (field === 'rbw') {
    const mode = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
    if (mode) mode.value = 'manual';
    applyRBW();
  } else if (field === 'vbw') {
    const mode = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
    if (mode) mode.value = 'manual';
    applyVBW();
  } else if (field === 'pnm' && S.measOn && S.measTabSel === 'pnm') {
    measPnmApply();
  }
}

function validateFrequencyWindow(window: unknown, ids: string[]): boolean {
  for (const id of ids) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    if (input) input.setCustomValidity(window ? '' : 'Invalid frequency range');
  }
  if (!window) {
    (document.getElementById(ids[0]) as HTMLInputElement | null)?.reportValidity();
    return false;
  }
  return true;
}

export function applyCenterSpan() {
  const window = normalizeCenterSpan(
    parseFreqUnit('center'), parseFreqUnit('span'), S.FREQ_MIN, S.FREQ_MAX);
  if (!validateFrequencyWindow(window, ['input-center', 'input-span'])) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', center: window!.center, span: window!.span });
}

export function applyStartStop() {
  const window = normalizeStartStop(
    parseFreqUnit('start'), parseFreqUnit('stop'), S.FREQ_MIN, S.FREQ_MAX);
  if (!validateFrequencyWindow(window, ['input-start', 'input-stop'])) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', start: window!.start, stop: window!.stop });
}
export function applyFullSpan() {
  beginFrequencyCommit('swp-freq-settings');
  send({
    cmd: 'SET_FREQ',
    center: (S.FREQ_MIN + S.FREQ_MAX) / 2,
    span: S.FREQ_MAX - S.FREQ_MIN,
  });
}

function formatSpanStep(value: number): string {
  if (value >= 100) return value.toFixed(0);
  if (value >= 10) return value.toFixed(1).replace(/\.0$/, '');
  return value.toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
}

export function syncSwpSpanStep(swpSpan = S.spanHz) {
  if (S.spanStepAuto) S.setSpanStepHz(niceSpanStep(swpSpan));
  const input = document.getElementById('input-span-step') as HTMLInputElement | null;
  const unit = document.getElementById('span-step-unit');
  if (input && document.activeElement !== input) {
    input.value = formatSpanStep(toUnit(S.spanStepHz, 'span'));
  }
  if (unit) unit.textContent = S.units.span;
  const auto = document.getElementById('btn-span-step-auto');
  if (auto) auto.classList.toggle('active', S.spanStepAuto);
}

export function updateCustomSpanStep() {
  const input = document.getElementById('input-span-step') as HTMLInputElement | null;
  if (!input) return;
  const value = Number(input.value);
  if (!isFinite(value) || value <= 0) return;
  const scale = S.units.span === 'GHz' ? 1e9 : S.units.span === 'MHz' ? 1e6
    : S.units.span === 'kHz' ? 1e3 : 1;
  S.setSpanStepAuto(false);
  S.setSpanStepHz(Math.max(100, value * scale));
  syncSwpSpanStep();
}

export function resetSpanStepAuto() {
  S.setSpanStepAuto(true);
  syncSwpSpanStep();
}

export function stepSwpSpan(direction: -1 | 1) {
  const targetSpan = steppedSpan(
    S.spanHz,
    S.spanStepHz,
    direction,
    100,
    S.FREQ_MAX - S.FREQ_MIN,
  );
  if (targetSpan === S.spanHz) return;
  const window = normalizeCenterSpan(
    S.centerHz, targetSpan, S.FREQ_MIN, S.FREQ_MAX);
  if (!window) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', center: window.center, span: window.span });
}

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  const value = parseFloat(el.value);
  if (!isFinite(value)) return;
  send({ cmd: 'SET_REF', mode: 'manual', ref: value });
}

const REF_MIN = -50;
const REF_MAX = 30;

// Pending Ref target while a step command is in flight (see steppedRefLevel).
let refPending: number | null = null;
let refPendingAt = 0;

export function refStepDbm(): number {
  // One full grid division: ▲/▼ moves Ref by the current dB-per-division value.
  return S.dbPerDiv;
}

export function adjustRefLevel(direction: -1 | 1) {
  const base = refPending ?? S.refLevel;
  const next = steppedRefLevel(base, refStepDbm(), direction, REF_MIN, REF_MAX);
  if (next === base) return;
  refPending = next;
  refPendingAt = Date.now();
  send({ cmd: 'SET_REF', mode: 'manual', ref: next });
}

export function syncRefLevelStatus(responseTo?: string) {
  if (refPending === null) return;
  if (responseTo === 'SET_REF' || Date.now() - refPendingAt > 2500) refPending = null;
}

export function setRefAuto() {
  if (S.refMode === 'auto') {
    send({ cmd: 'SET_REF', mode: 'manual', ref: S.refLevel });
  } else {
    send({ cmd: 'SET_REF', mode: 'auto' });
  }
}
export function setScale(v: number) {
  S.setDbPerDiv(v);
  syncScaleButtons();
  updateInfoBar();
  renderAll();
}
export function syncScaleButtons() {
  const grp = document.getElementById('unit-scale-group');
  if (!grp) return;
  for (const b of grp.children) (b as HTMLElement).classList.toggle('active', parseFloat(b.textContent || '') === S.dbPerDiv);
}
export function applyRBW() {
  const sel = document.getElementById('select-rbw-mode') as HTMLSelectElement;
  const rbwMode = sel?.value || 'auto';
  S.setRbwMode(rbwMode);
  const m: any = { cmd: 'SET_RBW', mode: rbwMode };
  if (rbwMode === 'manual') m.rbw = parseFreqUnit('rbw');
  if (S.rtaMode) clearRtaAccum();
  send(m);
}
export function applyVBW() {
  const sel = document.getElementById('select-vbw-mode') as HTMLSelectElement;
  const vbwMode = sel?.value || 'bypass';
  S.setVbwMode(vbwMode);
  const m: any = { cmd: 'SET_VBW', mode: vbwMode };
  if (vbwMode === 'manual') m.vbw = parseFreqUnit('vbw');
  if (S.rtaMode) clearRtaAccum();
  send(m);
}
export function applyPoints() {
  const el = document.getElementById('input-points') as HTMLInputElement;
  send({ cmd: 'SET_POINTS', points: parseInt(el?.value || '1001') || 1001 });
}
export function setSpurMode(mode: string) { send({ cmd: 'SET_SPUR', mode }); }
export function setWindow(v: string) { send({ cmd: 'SET_WINDOW', window: parseInt(v) }); }
export function setRefClock(mode: string) {
  send({ cmd: 'SET_REFCK', mode });
}
export function toggleRefClkOut() {
  const btn = document.getElementById('btn-refclk-out');
  const cur = btn && btn.classList.contains('on');
  send({ cmd: 'SET_REFCKOUT', on: !cur });
}
export function syncRefClkOut(s: any) {
  const btn = document.getElementById('btn-refclk-out');
  if (!btn) return;
  const on = !!s.refclk_out;
  btn.classList.toggle('on', on);
  btn.textContent = on ? (t('output') + ': ' + t('on')) : (t('output') + ': ' + t('off'));
}
export function setAmp() {
  send({
    cmd: 'SET_AMP',
    atten: parseInt((document.getElementById('select-atten') as HTMLSelectElement).value),
    preamp: parseInt((document.getElementById('select-preamp') as HTMLSelectElement).value),
    ifgain: parseInt((document.getElementById('select-ifgain') as HTMLSelectElement).value),
    gain_strategy: parseInt((document.getElementById('select-gainstrategy') as HTMLSelectElement).value),
  });
}
export function setOffset() {
  const v = parseFloat((document.getElementById('input-offset') as HTMLInputElement).value);
  S.setDisplayOffset(isFinite(v) ? v : 0);
  renderAll();
}
export function toggleGapFill() {
  S.setCurrentGapFill(!S.currentGapFill);
  const btn = document.getElementById('btn-gapfill');
  if (btn) {
    btn.textContent = S.currentGapFill ? t('on') : t('off');
    btn.classList.toggle('active', S.currentGapFill);
  }
}
export function connectDevice() { send({ cmd: 'CONNECT' }); }

// ── Marker operations ──
export function activeMarkerPeak() {
  const p = getDisplayPowers(); if (!p) return;
  let bi = 0, bv = -Infinity;
  for (let i = 0; i < p.length; i++) { const v = p[i]; if (isFinite(v) && v > bv) { bv = v; bi = i; } }
  setMarkerIdx(bi);
}
export function activeMarkerValley() {
  const p = getDisplayPowers(); if (!p) return;
  let bi = 0, bv = Infinity;
  for (let i = 0; i < p.length; i++) { const v = p[i]; if (isFinite(v) && v < bv) { bv = v; bi = i; } }
  const list = findExtremesOrderedPub('right', false);
  S.setValleySeqPos(list.findIndex((x: any) => Math.abs(x.i - bi) <= 3));
  if (S.valleySeqPos < 0) S.setValleySeqPos(0);
  const fit = parabolaFitPub(p, bi);
  const bh = (S.freqArray && S.freqArray.length > 1) ? S.freqArray[1] - S.freqArray[0] : 0;
  setMarkerIdx(bi, S.freqArray![bi] + fit.dk * bh);
}
export function activeMarkerNextPeakLeft() { nextExtreme('left', true); }
export function activeMarkerNextPeakRight() { nextExtreme('right', true); }
export function activeMarkerNextValleyLeft() { nextExtreme('left', false); }
export function activeMarkerNextValleyRight() { nextExtreme('right', false); }
export function markerToCenter() {
  const m = S.markers.find(x => x.id === S.activeMkrId);
  if (!m || !m.enabled) return;
  const center = markerFreqHz(m.idx);
  if (S.rtaMode) {
    S.setRtaCenterHz(center);
    send({ cmd: 'SET_RTA', center });
  } else {
    S.setCenterHz(center);
    updateFreqUIInputs();
    send({ cmd: 'SET_FREQ', center: S.centerHz, span: S.spanHz });
  }
}
export function selectMarker(id: number) {
  S.setActiveMkrId(id);
  document.querySelectorAll('.mkr-btn').forEach(b => b.classList.remove('active'));
  const b = document.querySelector(`.mkr-btn.M${id}`);
  if (b) b.classList.add('active');
  const m = S.markers.find(x => x.id === id);
  if (!m) return;
  m.enabled = true;
  if (m.mode === 'OFF') m.mode = 'NORMAL';
  autoTrackMarker(m);
  syncMarkerTrackingToggle();
  renderAll();
}

function autoTrackMarker(m: S.MarkerState) {
  assignMarkerToBestPeak(m);
}

export function syncMarkerTrackingToggle() {
  const marker = S.markers.find(item => item.id === S.activeMkrId);
  const button = document.getElementById('btn-marker-tracking');
  if (!marker || !button) return;
  button.textContent = t('tracking');
  button.classList.toggle('active', marker.tracking);
  button.setAttribute('aria-pressed', String(marker.tracking));
}

export function toggleActiveMarkerTracking() {
  const marker = S.markers.find(item => item.id === S.activeMkrId);
  if (!marker) return;
  toggleMarkerTracking(marker);
  syncMarkerTrackingToggle();
  renderAll();
}

// Graph mode changes are committed only after the backend STATUS confirms them.
let graphModePending = false;
let graphModeTarget: 'std' | 'rta' | 'sdr' = 'std';
let confirmedGraphMode = '';

export function currentGraphMode(): string { return confirmedGraphMode || 'std'; }

export function setGraphMode(mode: string) {
  const target: 'std' | 'rta' | 'sdr' =
    mode === 'rta' ? 'rta' : mode === 'sdr' ? 'sdr' : 'std';
  if (graphModePending || target === confirmedGraphMode) return;
  if (target !== 'std' && S.measOn) {
    exitMeasModePub(false);
    S.setMeasOn(false);
    const button = document.getElementById('btn-meas-onoff');
    if (button) button.textContent = t('off');
    setMeasButtons(false);
  }
  graphModePending = true;
  graphModeTarget = target;
  const modeButton = document.getElementById('btn-mode-rta') as HTMLButtonElement | null;
  const sdrButton = document.getElementById('btn-mode-sdr') as HTMLButtonElement | null;
  if (modeButton) modeButton.disabled = true;
  if (sdrButton) sdrButton.disabled = true;
  // Clicking the SDR button is a user gesture: start/resume browser audio here.
  setSdrAudioEnabled(target === 'sdr');
  send({ cmd: 'SET_MODE', mode: target });
}

export function releaseGraphModePending() {
  graphModePending = false;
  graphModeTarget = 'std';
  const modeButton = document.getElementById('btn-mode-rta') as HTMLButtonElement | null;
  const sdrButton = document.getElementById('btn-mode-sdr') as HTMLButtonElement | null;
  if (modeButton) modeButton.disabled = false;
  if (sdrButton) sdrButton.disabled = false;
}

export function syncGraphModeStatus(mode: string) {
  if (mode !== 'std' && mode !== 'rta' && mode !== 'sdr') return;
  if (graphModePending && mode !== graphModeTarget) return;
  if (graphModePending) releaseGraphModePending();
  if (confirmedGraphMode === mode) return;
  confirmedGraphMode = mode;
  const isRtaLike = mode === 'rta' || mode === 'sdr';
  const isSdr = mode === 'sdr';
  S.setRtaMode(isRtaLike);
  S.setViewMode(isRtaLike ? 'rta' : 'std');
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
  renderAll();
}

function clearRtaAccum() {
  // A reconfiguration (span/rbw/sweep) invalidates every accumulation on the old
  // frequency axis / resolution: probability density, per-trace displays, waterfall.
  if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
  for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
  for (let ti = 0; ti < S.rtaAvgN.length; ti++) S.rtaAvgN[ti] = 0;
  S.resetWaterfall();
}

// Density persistence/grain restored on page load + RTA entry (independent memory keys)
export function restoreRtaDensityCfg() {
  const f = localStorage.getItem('rta-fade');
  if (f) { const s = document.getElementById('select-rta-fade') as HTMLSelectElement | null; if (s) s.value = f; S.setRtaFade(parseFloat(f)); }
  const bn = localStorage.getItem('rta-bins');
  if (bn) { const s = document.getElementById('select-rta-bins') as HTMLSelectElement | null; if (s) s.value = bn; S.setRtaAmpBins(parseInt(bn) || 128); }
}

// Density grain: changing bins invalidates the current density array (ws.ts rebuilds it
// automatically on the next frame because the length no longer matches).
export function setRtaBins(bins: number) {
  S.setRtaAmpBins(bins);
  if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
  try { localStorage.setItem('rta-bins', String(bins)); } catch { /* ignore */ }
  renderAll();
}

// Step the RTA span one notch (delta: +1 narrower ▼, -1 wider ▲) or jump to full.
export function rtaSpanStep(delta: number) {
  const sel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  if (!sel) return;
  const idx = Array.from(sel.options).findIndex(o => o.value === sel.value);
  const ni = Math.max(0, Math.min(sel.options.length - 1, idx + delta));
  if (ni === idx || ni < 0) return;
  sel.value = sel.options[ni].value;
  applyRta();
}
export function rtaSpanFull() {
  const sel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  if (!sel || sel.options.length === 0) return;
  sel.value = sel.options[0].value;   // options are sorted largest first (50.8M)
  applyRta();
}

export function applyRta() {
  const c = parseFloat((document.getElementById('input-rta-center') as HTMLInputElement).value || '1000');
  const u = S.units.rta_center || 'MHz';
  const requestedCenter = isFinite(c)
    ? (u === 'GHz' ? c * 1e9 : u === 'kHz' ? c * 1e3 : c * 1e6)
    : 1e9;
  const spanEl = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  const requestedSpan = spanEl ? (parseFloat(spanEl.value) || 50781250) : 50781250;
  const window = normalizeCenterSpan(
    requestedCenter, requestedSpan, S.FREQ_MIN, S.FREQ_MAX, 1000);
  if (!validateFrequencyWindow(window, ['input-rta-center'])) return;
  clearRtaAccum();
  beginFrequencyCommit('rta-freq-settings');
  send({ cmd: 'SET_RTA', center: window!.center, span: window!.span });
}

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
  send({ cmd: 'SET_SDR', center: centerMhz * 1e6, decimate });
}

export function applySdrTune() {
  const listenMhz = sdrNumber('input-sdr-listen', 1000);
  send({ cmd: 'SET_SDR_TUNE', listen: listenMhz * 1e6 });
}

export function applySdrDemod() {
  const mode = (document.getElementById('select-sdr-demod') as HTMLSelectElement | null)?.value || 'am';
  const ifbw = sdrNumber('select-sdr-ifbw', 6000);
  const volume = sdrNumber('input-sdr-volume', 0.8);
  const squelch = sdrNumber('input-sdr-squelch', -110);
  send({ cmd: 'SET_SDR_DEMOD', mode, ifbw, volume, squelch, agc: sdrAgcOn() });
}

export function toggleSdrAgc(el: HTMLElement) {
  const on = !el.classList.contains('active');
  el.classList.toggle('active', on);
  el.textContent = on ? t('on') : t('off');
  send({ cmd: 'SET_SDR_DEMOD', agc: on });
}

// SDR status -> panel readouts (called on every STATUS)
function sdrSet(id: string, value: string) {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  if (el && document.activeElement !== el) el.value = value;
}

export function syncSdrPanel(s: any) {
  const sdr = s?.sdr;
  if (!sdr) return;
  sdrSet('input-sdr-center', (Number(sdr.center) / 1e6).toFixed(4));
  sdrSet('select-sdr-decimate', String(sdr.decimate));
  sdrSet('input-sdr-listen', (Number(sdr.listen) / 1e6).toFixed(4));
  sdrSet('select-sdr-demod', sdr.demod);
  sdrSet('select-sdr-ifbw', String(Math.round(Number(sdr.if_bw))));
  sdrSet('input-sdr-volume', String(sdr.volume));
  sdrSet('input-sdr-squelch', String(Math.round(Number(sdr.squelch))));
  const agc = document.getElementById('btn-sdr-agc');
  if (agc) {
    agc.textContent = sdr.agc ? t('on') : t('off');
    agc.classList.toggle('active', !!sdr.agc);
  }
  const lvl = document.getElementById('cur-sdr-level');
  if (lvl) lvl.textContent = Number.isFinite(sdr.level_dbfs) ? sdr.level_dbfs.toFixed(1) + ' dBFS' : '';
  const adm = document.getElementById('cur-sdr-adm');
  if (adm) {
    const m = sdr.adm || {};
    if (m.kind === 'am') {
      adm.textContent = `AM m=${(m.mod_depth ?? 0).toFixed(0)}%  SINAD ${(m.sinad ?? 0).toFixed(1)}  SNR ${(m.snr ?? 0).toFixed(1)} dB`;
    } else if (m.kind === 'fm') {
      adm.textContent = `FM dev=${(m.deviation ?? 0).toFixed(0)} Hz  SINAD ${(m.sinad ?? 0).toFixed(1)}  SNR ${(m.snr ?? 0).toFixed(1)} dB`;
    } else {
      adm.textContent = '';
    }
  }
}
export function toggleWaterfall() {
  S.setWaterfallOn(!S.waterfallOn);
  if (S.waterfallOn) S.resetWaterfall();
  const wf = document.getElementById('waterfall');
  if (wf) wf.style.display = S.waterfallOn ? '' : 'none';
  const mt = document.getElementById('marker-table');
  if (mt) mt.style.display = S.waterfallOn ? 'none' : '';
  const btn = document.getElementById('btn-waterfall');
  if (btn) btn.classList.toggle('active', S.waterfallOn);   // text stays "Waterfall", active = on
  renderAll();
}
export function toggleWfPause() {
  S.setWfPaused(!S.wfPaused);
  const b = document.getElementById('btn-wf-pause');
  if (b) b.classList.toggle('active', S.wfPaused);
}
export function resetWf() {
  S.resetWaterfall();
  renderAll();
}
export function setSweepSpeed() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const mode = parseInt(sel?.value || '0');
  const time = parseFloat(tin?.value || '0');
  const m: any = { cmd: 'SET_SWEEP', mode };
  if (mode === 6 || mode === 7 || mode === 8) {
    const minimum = mode === 7 ? 0.001 : 1;
    m.time = Math.max(minimum, isFinite(time) ? time : minimum);
  }
  send(m);
}
// 仅 ×N(6)/Manual(7) 需要输入框+Set 按钮; 其余固定档隐藏
export function syncSweepInput() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const btn = document.querySelector('button[data-action="set-sweep"]') as HTMLElement;
  if (!sel) return;
  const mode = parseInt(sel.value || '0');
  const need = mode >= 6;   // minSWTxN(6)/Manual(7)/minSMPxN(8) need an input value
  if (tin) { tin.style.display = need ? '' : 'none'; tin.placeholder = mode === 7 ? t('swt_sec') : t('swt_xn'); }
  if (btn) btn.style.display = need ? '' : 'none';
}

// Turn all markers on/off at once (toggle)
export function updateMarkersAllBtn() {
  const allOn = S.markers.every(m => m.enabled && m.mode !== 'OFF');
  const el = document.getElementById('btn-markers-all');
  if (el) {
    el.textContent = allOn ? t('all_on') : t('all_off');
    el.classList.toggle('active', allOn);
  }
}
export function toggleMarkersAll() {
  const allOn = S.markers.every(m => m.enabled && m.mode !== 'OFF');
  if (allOn) {
    S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; });
  } else {
    // All on: track one peak at a time — M1 takes the strongest peak, later markers skip
    // occupied positions and take the next strongest
    // (consistent with the autoTrackMarker logic in selectMarker)
    S.markers.forEach(m => { m.enabled = true; if (m.mode === 'OFF') m.mode = 'NORMAL'; });
    S.markers.forEach(m => autoTrackMarker(m));
  }
  updateMarkersAllBtn();
  const themeBtn = document.getElementById('btn-theme');
  if (themeBtn) themeBtn.textContent = getTheme() === 'dark' ? t('dark') : t('light');
  syncMarkerTrackingToggle();
  renderAll();
}

// Preset
export function presetAll() {
  exitMeasModePub();
  S.setMeasOn(false);
  const b = document.getElementById('btn-meas-onoff');
  if (b) b.textContent = t('off');
  setMeasButtons(false);
  S.setDisplayRef(0);
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
  S.setDefaultMkrDone(true);
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
  send({ cmd: 'SET_PRESET' });
  updateInfoBar(); applyMeasUI(); renderAll();
}

// Current frontend time (shown when not locked)
function fmtNow(): string {
  const d = new Date();
  const p2 = (n: number) => String(n).padStart(2, '0');
  return d.getFullYear() + '-' + p2(d.getMonth() + 1) + '-' + p2(d.getDate()) + ' ' +
    p2(d.getHours()) + ':' + p2(d.getMinutes()) + ':' + p2(d.getSeconds());
}

// GNSS detail popover: fill + show/close
export function fillGnssDetail() {
  const g = S.lastGnss || {};
  const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  set('gnss-d-lock', g.lock ? t('status_locked') : t('status_nolock'));
  set('gnss-d-sats', g.sats != null ? String(g.sats) : '-');
  set('gnss-d-docxo', g.docxo ? t('on') : t('off'));
  set('gnss-d-docxo_mode', g.docxo_mode === 0 ? t('gnss_docxo_lock') : (g.docxo_mode === 1 ? t('gnss_docxo_hold') : '-'));
  set('gnss-d-antenna', g.antenna === 0 ? t('gnss_ext_ant') : (g.antenna === 1 ? t('gnss_int_ant') : '-'));
  set('gnss-d-latitude', g.latitude != null && Math.abs(g.latitude) > 0.0001 ? g.latitude.toFixed(6) + '°' : '-');
  set('gnss-d-longitude', g.longitude != null && Math.abs(g.longitude) > 0.0001 ? g.longitude.toFixed(6) + '°' : '-');
  set('gnss-d-altitude', g.altitude != null ? String(g.altitude) + ' m' : '-');
  // Time: GNSS UTC (shown only when locked and valid) + system local time (always shown)
  set('gnss-d-utc', (g.lock && g.time && !g.time.includes('0000')) ? g.time : '-');
  set('gnss-d-local', fmtNow());
  const pop = document.getElementById('gnss-popover');
  if (pop) pop.style.display = '';
}
export function closeGnssDetail() {
  const pop = document.getElementById('gnss-popover');
  if (pop) pop.style.display = 'none';
}

// Sync all toggle button texts when the language changes
export function syncToggleTexts() {
  const pl = document.getElementById('btn-peaklist');
  if (pl) pl.textContent = S.peakListOn ? t('on') : t('off');
  const gf = document.getElementById('btn-gapfill');
  if (gf) gf.textContent = S.currentGapFill ? t('on') : t('off');
  const mo = document.getElementById('btn-meas-onoff');
  if (mo) mo.textContent = S.measOn ? t('on') : t('off');
  const rc = document.getElementById('btn-refclk-out');
  if (rc) {
    const on = rc.classList.contains('on');
    rc.textContent = on ? (t('output') + ': ' + t('on')) : (t('output') + ': ' + t('off'));
  }
  updateMarkersAllBtn();
  syncMarkerTrackingToggle();
  const tracking = document.getElementById('btn-marker-tracking');
  if (tracking) tracking.textContent = t('tracking');
  const wb = document.getElementById('btn-waterfall');
  if (wb) wb.classList.toggle('active', S.waterfallOn);
}

// ── Panel collapse ──
export function toggleGroup(el: HTMLElement) {
  const g = el.closest('.control-group');
  if (g) {
    g.classList.toggle('collapsed');
    syncToggleIcons();
  }
}
export function toggleAllGroups() {
  const groups = document.querySelectorAll('.control-group');
  const allCollapsed = Array.from(groups).every(g => g.classList.contains('collapsed'));
  groups.forEach(g => g.classList.toggle('collapsed', !allCollapsed));
  syncToggleIcons();
}
export function syncToggleIcons() {
  const groups = Array.from(document.querySelectorAll('.control-group'));
  const allCollapsed = groups.length && groups.every(g => g.classList.contains('collapsed'));
  document.querySelectorAll('.global-toggle').forEach(b => { b.textContent = allCollapsed ? '^' : 'v'; });
  groups.forEach(g => {
    const b = g.querySelector('.panel-toggle');
    if (b) b.textContent = g.classList.contains('collapsed') ? '+' : '-';
  });
}

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
    'set-smooth': (el) => { S.setSmoothBins(parseInt((el as HTMLSelectElement).value) || 1); renderAll(); },
    'set-norm-refwin': (el) => {
      const v = parseInt((el as HTMLSelectElement).value) || 0;
      setNormRefWinUser(v);
      const t = S.traces[S.activeTraceIdx];
      if (t.reference && t.isNormalized && t.powers) {
        const ref = buildReferenceTablePub(t.powers);
        t.reference = smoothRefWindow(ref, normRefWindow());
      }
      renderAll();
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
    'apply-sdr-tune': () => applySdrTune(),
    'set-sdr-demod': () => applySdrDemod(),
    'toggle-sdr-agc': (el) => toggleSdrAgc(el),
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
export function bindCanvas() {
  const canvas = document.getElementById('spectrum') as HTMLCanvasElement;
  if (!canvas) return;
  canvas.addEventListener('mousedown', (e) => {
    const p = getDisplayPowers(); if (!p) return;
    const rect = canvas.getBoundingClientRect();
    const pr = plotRectPub();
    const x = (e.clientX - rect.left) * (S.W / rect.width);
    if (x < pr.x || x > pr.x + pr.w) return;
    S.setDragging(true);
    placeMarkerFromX(x, p);
  });
  window.addEventListener('mousemove', (e) => {
    if (!S.dragging) return;
    const p = getDisplayPowers(); if (!p) return;
    const rect = canvas.getBoundingClientRect();
    const x = (e.clientX - rect.left) * (S.W / rect.width);
    placeMarkerFromX(x, p);
  });
  window.addEventListener('mouseup', () => S.setDragging(false));
}
function placeMarkerFromX(x: number, p: Float32Array) {
  const pr = plotRectPub();
  const frac = (x - pr.x) / pr.w;
  setMarkerIdx(Math.round(frac * (p.length - 1)));
}

import { plotRect as plotRectPub } from '../render/plot';
import { findExtremesOrdered as findExtremesOrderedPub, parabolaFit as parabolaFitPub } from '../dsp/peaks';
import { exitMeasMode as exitMeasModePub } from './measure';
