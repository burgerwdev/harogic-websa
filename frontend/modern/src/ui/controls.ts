// Control commands + data-action binding + panel collapse + marker ops + canvas interaction
import * as S from '../core/store';
import { send } from '../core/wsSend';
import { updateFreqUIInputs } from '../core/ws';
import { updateInfoBar } from '../render/infobar';
import { renderAll } from '../render/spectrum';
import { getDisplayPowers, nextExtreme, setMarkerIdx, getTraceDisplay } from '../dsp/peaks';
import { markerFreqHz } from '../core/markerCommon';
import { parseFreqUnit, toUnit } from '../core/units';
import { setSmoothBins } from '../core/store';
import { normRefWindow, setNormRefWinUser, smoothRefWindow, buildReferenceTablePub } from './normPub';
import { switchTraceTab, toggleFreeze, setTraceMode, clearRtaTrace } from './traceOps';
import { normalizeActiveTrace, resetActiveTraceNormalize, updateNormalizeStatusUI } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
import { togglePeakList, peakThrManual, peakThrAuto } from '../render/peaklist';
import { measToggle, measTab, applyMeasUI, setMeasButtons } from './measure';
import { measureAmp, clearAmp } from '../meas/amplitude';
import { measHarmApply, autoHarmSpan } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';
import { canvasColors } from '../core/theme';
import { t } from '../core/i18n';

// ── Frequency linking ──
export function fitSpan(center: number, span: number): number {
  const maxSpan = 2 * Math.min(center - S.FREQ_MIN, S.FREQ_MAX - center);
  return Math.max(1000, Math.min(span, maxSpan));
}
export function linkFreq(field: string) {
  const v = parseFreqUnit(field);
  if (!isFinite(v) || v <= 0) return;
  if (field === 'center') {
    S.setCenterHz(v);
    S.setSpanHz(fitSpan(S.centerHz, S.spanHz));
  } else if (field === 'span') {
    S.setSpanHz(fitSpan(S.centerHz, v));
  } else if (field === 'start') {
    const stop = S.centerHz + S.spanHz / 2;
    S.setCenterHz((v + stop) / 2);
    S.setSpanHz(fitSpan(S.centerHz, stop - v));
  } else if (field === 'stop') {
    const start = S.centerHz - S.spanHz / 2;
    S.setCenterHz((start + v) / 2);
    S.setSpanHz(fitSpan(S.centerHz, v - start));
  }
  if (S.spanHz <= 0) S.setSpanHz(1000);
  S.setCenterHz(Math.max(S.FREQ_MIN + S.spanHz / 2, Math.min(S.FREQ_MAX - S.spanHz / 2, S.centerHz)));
  updateFreqUIInputs();
  send({ cmd: 'SET_FREQ', center: S.centerHz, span: S.spanHz });
}
export function applyCenterSpan() {
  S.setCenterHz(parseFreqUnit('center'));
  S.setSpanHz(parseFreqUnit('span'));
  if (S.spanHz <= 0) S.setSpanHz(1000);
  S.setSpanHz(fitSpan(S.centerHz, S.spanHz));
  S.setCenterHz(Math.max(S.FREQ_MIN + S.spanHz / 2, Math.min(S.FREQ_MAX - S.spanHz / 2, S.centerHz)));
  updateFreqUIInputs();
  send({ cmd: 'SET_FREQ', center: S.centerHz, span: S.spanHz });
}
export function applyStartStop() {
  let start = Math.max(100000, parseFreqUnit('start'));
  let stop = parseFreqUnit('stop');
  if (stop <= start) stop = start + 1000;
  S.setCenterHz((start + stop) / 2);
  S.setSpanHz(fitSpan(S.centerHz, stop - start));
  updateFreqUIInputs();
  send({ cmd: 'SET_FREQ', center: S.centerHz, span: S.spanHz });
}
export function applyFullSpan() { send({ cmd: 'SET_FREQ', center: 4.50005e9, span: 8.9999e9 }); }

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  const v = parseFloat(el.value);
  if (!isFinite(v)) return;
  // ref level = pure display scale (device-reported power already includes atten compensation):
  // only update the display reference + redraw, do not send to the device → no device reconfig
  // (auto atten stays stable, no stutter), and it is unaffected by the device firmware's
  // ref-atten coupling; the atten feature (SET_AMP) remains fully independent
  S.setDisplayRef(v);
  S.setRefUserSet(true);
  updateInfoBar();
  renderAll();
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
  if (S.rtaMode) { clearRtaAccum(); rememberRtaSettings(); }
  send(m);
}
export function applyVBW() {
  const sel = document.getElementById('select-vbw-mode') as HTMLSelectElement;
  const vbwMode = sel?.value || 'bypass';
  S.setVbwMode(vbwMode);
  const m: any = { cmd: 'SET_VBW', mode: vbwMode };
  if (vbwMode === 'manual') m.vbw = parseFreqUnit('vbw');
  if (S.rtaMode) { clearRtaAccum(); rememberRtaSettings(); }
  send(m);
}
export function applyPoints() {
  const el = document.getElementById('input-points') as HTMLInputElement;
  send({ cmd: 'SET_POINTS', points: parseInt(el?.value || '1001') || 1001 });
}
export function setSpurMode(mode: string) { send({ cmd: 'SET_SPUR', mode }); }
export function setWindow(v: string) { send({ cmd: 'SET_WINDOW', window: parseInt(v) }); }
export function setRefClock(mode: string) { send({ cmd: 'SET_REFCK', mode }); }
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
  S.setCenterHz(markerFreqHz(m.idx));
  updateFreqUIInputs();
  send({ cmd: 'SET_FREQ', center: S.centerHz, span: S.spanHz });
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
  renderAll();
}

function autoTrackMarker(m: S.MarkerState) {
  const t = S.traces[S.activeTraceIdx];
  const p = (t && t.powers) ? t.powers : getDisplayPowers();
  if (!p || !S.freqArray) return;
  const occupied = S.markers.filter(x => x.enabled && x.id !== m.id && x.mode !== 'OFF').map(x => x.idx);
  const thrEl = document.getElementById('input-peakthr') as HTMLInputElement;
  const thr = thrEl ? (parseFloat(thrEl.value) || -200) : -200;
  const peaks: [number, number][] = [];
  for (let i = 1; i < p.length - 1; i++) {
    const v = p[i];
    if (!isFinite(v)) continue;
    if (v > thr && v >= p[i - 1] && v > p[i + 1]) peaks.push([i, v]);
  }
  const free = peaks.filter(pk => !occupied.some(o => Math.abs(o - pk[0]) <= 5));
  free.sort((a, b) => b[1] - a[1]);
  if (free.length) {
    m.idx = free[0][0];
    m.freq = S.freqArray[free[0][0]];
  } else if (peaks.length) {
    m.idx = peaks[0][0];
    m.freq = S.freqArray[peaks[0][0]];
  }
}

// Graph mode: RTA toggle (SWP is the default; click RTA to enter/exit)
export function setGraphMode(mode: string) {
  const isRta = mode === 'rta';
  S.setRtaMode(isRta);
  if (isRta && S.measOn) {
    exitMeasModePub();
    S.setMeasOn(false);
    const b = document.getElementById('btn-meas-onoff');
    if (b) b.textContent = t('off');
    setMeasButtons(false);
  }
  if (isRta) {
    S.setViewMode('rta');
    S.resetWaterfall();
    send({ cmd: 'SET_MODE', mode: 'rta' });
  } else {
    if (S.viewMode === 'rta') send({ cmd: 'SET_MODE', mode: 'std' });
    S.setViewMode('std');
  }
  const bRta = document.getElementById('btn-mode-rta');
  if (bRta) bRta.classList.toggle('active', isRta);
  localStorage.setItem('web-sa-mode', isRta ? 'rta' : 'std');
  // RTA mode: re-enter with the LAST session's settings if any (memorized), else defaults.
  if (isRta) {
    restoreRtaSettings();
  }
  // RTA mode: disable the Measurement panel + non-applicable trace/BW controls
  document.body.classList.toggle('rta-mode', isRta);
  const rtaDisable = [
    'select-smooth', 'select-refwin', 'btn-normalize',
    'select-window',
    'input-points', 'btn-points',
  ];
  // In RTA these STAY enabled (all independent per-session): RBW (select-rbw-mode/
  // btn-rbw-set/input-rbw -> set_rbw), VBW (select-vbw-mode/input-vbw/btn-vbw-set ->
  // set_vbw), sweep Speed (select-sweep-mode). Only FFT-window + Points are SWP-only.
  rtaDisable.forEach((id) => {
    const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
    if (el) el.disabled = isRta;
  });
  const resetBtn = document.querySelector('[data-action="reset-norm"]') as HTMLButtonElement | null;
  if (resetBtn) resetBtn.disabled = isRta;   // clear stays enabled in RTA
  // 频率控制区切换: RTA 专用设置 / SWP 常规设置
  const rtaF = document.getElementById('rta-freq-settings');
  const swpF = document.getElementById('swp-freq-settings');
  if (rtaF) rtaF.style.display = isRta ? '' : 'none';
  if (swpF) swpF.style.display = isRta ? 'none' : '';
  renderAll();
}
// --- RTA settings memory: re-entering RTA restores the previous session's config ---
const RTA_MEM = 'web-sa-rta';

function clearRtaAccum() {
  // A reconfiguration (span/rbw/sweep) invalidates every accumulation on the old
  // frequency axis / resolution: probability density, per-trace displays, waterfall.
  if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
  for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
  for (let ti = 0; ti < S.rtaAvgN.length; ti++) S.rtaAvgN[ti] = 0;
  S.resetWaterfall();
}

export function rememberRtaSettings() {
  const spanSel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  const rbwSel = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
  const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement | null;
  const cc = document.getElementById('input-rta-center') as HTMLInputElement | null;
  const vbwSel = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
  const mem = {
    span: spanSel ? spanSel.value : '50781250',
    rbwMode: rbwSel ? rbwSel.value : 'auto',
    vbwMode: vbwSel ? vbwSel.value : 'equal',
    sweepMode: sm ? sm.value : '2',
    center: cc ? cc.value : '1000',
  };
  try { localStorage.setItem(RTA_MEM, JSON.stringify(mem)); } catch { /* ignore */ }
}

function restoreRtaSettings() {
  let mem: any = null;
  try { mem = JSON.parse(localStorage.getItem(RTA_MEM) || 'null'); } catch { /* ignore */ }
  const spanSel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  const rbwSel = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
  const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const cc = document.getElementById('input-rta-center') as HTMLInputElement | null;
  const unit = S.units.rta_center || 'MHz';
  if (mem && spanSel && rbwSel && sm && cc) {
    spanSel.value = String(mem.span || '50781250');
    rbwSel.value = String(mem.rbwMode || 'auto');
    sm.value = String(mem.sweepMode || '2');
    cc.value = String(mem.center || '1000');
    syncSweepInput();
    const c = parseFloat(cc.value) || 1000;
    const center = unit === 'GHz' ? c * 1e9 : unit === 'kHz' ? c * 1e3 : c * 1e6;
    send({ cmd: 'SET_RTA', center, span: parseFloat(spanSel.value) || 50781250 });
    if (rbwSel.value === 'manual') {
      const iv = document.getElementById('input-rbw') as HTMLInputElement | null;
      send({ cmd: 'SET_RBW', mode: 'manual', rbw: parseFreqUnit('rbw') });
    } else {
      send({ cmd: 'SET_RBW', mode: 'auto' });
    }
    const vbwSel2 = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
    const vbwMode = String(mem.vbwMode || 'equal');
    if (vbwSel2) vbwSel2.value = vbwMode;
    const m2: any = { cmd: 'SET_VBW', mode: vbwMode };
    if (vbwMode === 'manual') {
      const iv = document.getElementById('input-vbw') as HTMLInputElement | null;
      m2.vbw = iv ? (parseFloat(iv.value) || 100) : 100;
    }
    send(m2);
    send({ cmd: 'SET_SWEEP', mode: parseInt(sm.value) || 2 });
  }
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
  const center = isFinite(c) ? (u === 'GHz' ? c * 1e9 : u === 'kHz' ? c * 1e3 : c * 1e6) : 1e9;
  const spanEl = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  const span = spanEl ? (parseFloat(spanEl.value) || 50781250) : 50781250;
  clearRtaAccum();
  rememberRtaSettings();
  send({ cmd: 'SET_RTA', center, span });
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
  if (mode === 6 || mode === 7 || mode === 8) m.time = isFinite(time) ? time : 0;
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
    S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; });
  } else {
    // All on: track one peak at a time — M1 takes the strongest peak, later markers skip
    // occupied positions and take the next strongest
    // (consistent with the autoTrackMarker logic in selectMarker)
    S.markers.forEach(m => { m.enabled = true; if (m.mode === 'OFF') m.mode = 'NORMAL'; });
    S.markers.forEach(m => autoTrackMarker(m));
  }
  updateMarkersAllBtn();
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
  S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; });
  S.setM3dB(null); S.setAmpRes(null); S.setHarm(null); S.setPnmData(null);
  S.setPeakListOn(false); S.setPeakMarks(null);
  const pl = document.getElementById('btn-peaklist');
  if (pl) pl.textContent = t('off');
  S.setActiveMkrId(1);
  S.setDefaultMkrDone(true);
  // Preset also resets the RTA session (backend reset_defaults) and clears the RTA
  // memory + UI so re-entering RTA starts from factory defaults.
  try { localStorage.removeItem(RTA_MEM); } catch { /* ignore */ }
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
    'set-ref-level': () => setRefLevel(),
    'apply-rbw': () => applyRBW(),
    'apply-vbw': () => applyVBW(),
    'apply-points': () => applyPoints(),
    'set-window': (el) => setWindow((el as HTMLSelectElement).value),
    'set-spur': (el) => setSpurMode((el as HTMLSelectElement).value),
    'set-refclock': (el) => setRefClock((el as HTMLSelectElement).value),
    'toggle-refclkout': () => toggleRefClkOut(),
    'set-amp': () => setAmp(),
    'set-trace-mode': (el) => setTraceMode((el as HTMLSelectElement).value),
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
    'meas-harm': () => measHarmApply(),
    'meas-pnm': () => measPnmApply(),
    'connect': () => connectDevice(),
    'gnss-detail': () => fillGnssDetail(),
    'gnss-close': () => closeGnssDetail(),
    'markers-all': () => toggleMarkersAll(),
    'set-sweep': () => { syncSweepInput(); setSweepSpeed(); },
    'toggle-rta': () => setGraphMode(S.rtaMode ? 'swp' : 'rta'),
    'apply-rta': () => applyRta(),
    'rta-span-down': () => rtaSpanStep(1),
    'rta-span-up': () => rtaSpanStep(-1),
    'rta-span-full': () => rtaSpanFull(),
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
    if (el) el.addEventListener('change', () => linkFreq(f));
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
