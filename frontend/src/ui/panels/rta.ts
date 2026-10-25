// RTA panel: analysis window, span stepping and the density background.
import * as S from '../../core/store';
import { normalizeCenterSpan } from '../../core/frequency';
import { beginFrequencyCommit, validateFrequencyWindow } from './frequency';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { rtaAmpBins, rtaDensContrast, rtaDensWidth, rtaDensLayers, rtaDensSource, rtaHalflife } from '../waterfallState';
import { units } from '../../core/units';

let axisTransition: { requestId: string; version: number; lo: number; hi: number; until: number } | null = null;

/** SET_RTA starts this wait; Clear and grain changes accept the next same-axis hit. */
export function beginRtaAxisTransition(requestId: string) {
  const frame = S.rtaData;
  if (frame) axisTransition = {
    requestId, version: frame.ver, lo: frame.startHz, hi: frame.stopHz, until: performance.now() + 3000,
  };
}

export function acceptRtaAxisFrame(version: number, lo: number, hi: number): boolean {
  if (!axisTransition) return true;
  if (performance.now() >= axisTransition.until || version !== axisTransition.version
      || Math.abs(lo - axisTransition.lo) > 0.5 || Math.abs(hi - axisTransition.hi) > 0.5) {
    axisTransition = null;
    return true;
  }
  return false;
}

export function rejectRtaAxisTransition(responseTo?: string, requestId?: string | number) {
  if (responseTo === 'SET_RTA' && requestId === axisTransition?.requestId) axisTransition = null;
}

export function resetRtaAxisTransition() {
  axisTransition = null;
}

export function clearRtaAccum() {
  // A reconfiguration (span/rbw/sweep) invalidates every accumulation on the old
  // frequency axis / resolution: probability density, per-trace displays, waterfall.
  S.setRtaDensity2d(null);
  for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
  for (let ti = 0; ti < S.rtaAvgN.length; ti++) {
    S.rtaAvgN[ti] = 0; S.rtaAvgSum[ti] = null; S.rtaDone[ti] = false;
  }
  S.resetWaterfall();
  requestRender();
}

// The afterglow gears are half-lives in seconds; Off (0) switches the density layer off
// entirely (see dsp/rtaDensity.ts).
/** The panel's own default gear, used when a value cannot be read at all. */
const RTA_HALFLIFE_DEFAULT = 0.5;
const RTA_HALFLIFE_GEARS = [0, 0.5, 2, 5, 10];

function normalizeHalflife(value: number): number {
  return RTA_HALFLIFE_GEARS.includes(value) ? value : RTA_HALFLIFE_DEFAULT;
}

/** Persist density settings across the slot, form and storage; zero is a valid Off value. */
export function setRtaHalflife(value: number) {
  const halflife = normalizeHalflife(value);
  rtaHalflife.set(halflife);
  if (!(halflife > 0)) S.setRtaDensity2d(null);
  const sel = document.getElementById('select-rta-fade') as HTMLSelectElement | null;
  if (sel) sel.value = String(halflife);
  try { localStorage.setItem('rta-halflife', String(halflife)); } catch { /* ignore */ }
  requestRender();
}

// Switching the source starts a new history: mixing the two statistics would be meaningless.
export function setRtaDensSource(source: 'trace' | 'live') {
  rtaDensSource.set(source);
  S.setRtaDensity2d(null);
  const sel = document.getElementById('select-rta-dens-source') as HTMLSelectElement | null;
  if (sel) sel.value = source;
  try { localStorage.setItem('rta-dens-source', source); } catch { /* ignore */ }
}

/** Layer visibility is display-only: it never touches trace modes, markers or history. */
export function setRtaDensLayers(layers: 'both' | 'density' | 'trace') {
  rtaDensLayers.set(layers);
  requestRender();
}

/** Contrast re-maps colours only; the history and the numbers behind it are untouched. */
export function setRtaDensContrast(contrast: number) {
  rtaDensContrast.set(isFinite(contrast) && contrast > 0 ? contrast : 1);
  requestRender();
}

/** The footprint is display-only: keep the source, samples and occupancy unchanged. */
export function setRtaDensWidth(width: number) {
  rtaDensWidth.set([1, 3, 5, 9].includes(width) ? width : 5);
  requestRender();
}

export function toggleDensitySettings() {
  const panel = document.getElementById('density-settings');
  if (!panel) return;
  panel.hidden = !panel.hidden;
  document.getElementById('btn-density-more')?.setAttribute('aria-expanded', String(!panel.hidden));
}

export function resetRtaDensityCfg() {
  rtaHalflife.reset();
  rtaDensSource.reset();
  rtaDensLayers.reset();
  rtaDensContrast.reset();
  rtaDensWidth.reset();
  rtaAmpBins.reset();
  rtaHalflife.confirm(RTA_HALFLIFE_DEFAULT);
  rtaDensSource.confirm('trace');
  rtaDensLayers.confirm('both');
  rtaDensContrast.confirm(1);
  rtaDensWidth.confirm(5);
  rtaAmpBins.confirm(128);
  S.setRtaDensity2d(null);
  for (const [id, value] of [
    ['select-rta-fade', rtaHalflife.get()],
    ['select-rta-dens-source', rtaDensSource.get()],
    ['select-rta-dens-layers', rtaDensLayers.get()],
    ['select-rta-dens-contrast', rtaDensContrast.get()],
    ['select-rta-dens-width', rtaDensWidth.get()],
    ['select-rta-bins', rtaAmpBins.get()],
  ] as const) {
    const select = document.getElementById(id) as HTMLSelectElement | null;
    if (select) select.value = String(value);
  }
  try {
    for (const key of ['rta-fade', 'rta-halflife', 'rta-dens-source', 'rta-bins']) localStorage.removeItem(key);
  } catch { /* Storage can be disabled. */ }
}

export function restoreRtaDensityCfg() {
  // The selects are the truth when nothing was ever stored (their own defaults are the
  // middle gears), so the value the density actually fades at and the value the panel shows
  // cannot disagree. A stored per-frame coefficient from an earlier release is dropped.
  try { localStorage.removeItem('rta-fade'); } catch { /* ignore */ }
  const fadeSel = document.getElementById('select-rta-fade') as HTMLSelectElement | null;
  const storedHalflife = localStorage.getItem('rta-halflife');
  const halflife = normalizeHalflife(Number(storedHalflife !== null ? storedHalflife : fadeSel?.value));
  if (fadeSel) fadeSel.value = String(halflife);
  rtaHalflife.set(halflife);
  const src = localStorage.getItem('rta-dens-source');
  if (src === 'trace' || src === 'live') {
    rtaDensSource.set(src);
    const srcSel = document.getElementById('select-rta-dens-source') as HTMLSelectElement | null;
    if (srcSel) srcSel.value = src;
  }
  const bn = localStorage.getItem('rta-bins');
  if (bn) { const s = document.getElementById('select-rta-bins') as HTMLSelectElement | null; if (s) s.value = bn; rtaAmpBins.set(parseInt(bn) || 128); }
}

// A grain change invalidates amplitude history immediately, including while paused.
export function setRtaBins(bins: number) {
  rtaAmpBins.set(bins);
  S.setRtaDensity2d(null);
  try { localStorage.setItem('rta-bins', String(bins)); } catch { /* ignore */ }
  requestRender();
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
  const u = units().rta_center || 'MHz';
  const requestedCenter = isFinite(c)
    ? (u === 'GHz' ? c * 1e9 : u === 'kHz' ? c * 1e3 : c * 1e6)
    : 1e9;
  const spanEl = document.getElementById('select-rta-span') as HTMLSelectElement | null;
  const requestedSpan = spanEl ? (parseFloat(spanEl.value) || 50781250) : 50781250;
  const window = normalizeCenterSpan(
    requestedCenter, requestedSpan, S.FREQ_MIN, S.FREQ_MAX, 1000);
  if (!validateFrequencyWindow(window, ['input-rta-center'])) return;
  clearRtaAccum();
  const request_id = beginFrequencyCommit('rta-freq-settings', window!);
  beginRtaAxisTransition(request_id);
  send({ cmd: 'SET_RTA', center: window!.center, span: window!.span, request_id });
}
