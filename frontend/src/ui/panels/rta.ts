// RTA panel: analysis window, span stepping and the density background.
import * as S from '../../core/store';
import { normalizeCenterSpan } from '../../core/frequency';
import { beginFrequencyCommit, validateFrequencyWindow } from './frequency';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { rtaAmpBins, rtaFade } from '../waterfallState';
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

// The four persistence gears, plus Off (0), which switches the density layer off entirely
// (see dsp/rtaDensity.ts).
export const RTA_FADE_OFF = 0;
/** The panel's own default gear, used when a value cannot be read at all. */
const RTA_FADE_DEFAULT = 0.975;

/** Persist density settings across the slot, form and storage; zero is a valid Off value. */
export function setRtaFade(value: number) {
  const fade = isFinite(value) ? value : RTA_FADE_DEFAULT;
  rtaFade.set(fade);
  if (!(fade > 0)) S.setRtaDensity2d(null);
  const sel = document.getElementById('select-rta-fade') as HTMLSelectElement | null;
  if (sel) sel.value = String(fade);
  try { localStorage.setItem('rta-fade', String(fade)); } catch { /* ignore */ }
  requestRender();
}

export function restoreRtaDensityCfg() {
  // The select is the truth when nothing was ever stored (its own default is the middle gear), so
  // the value the density actually fades at and the value the panel shows cannot disagree.
  const fadeSel = document.getElementById('select-rta-fade') as HTMLSelectElement | null;
  const storedFade = localStorage.getItem('rta-fade');
  const fade = Number(storedFade !== null ? storedFade : fadeSel?.value);
  if (isFinite(fade)) {
    if (fadeSel && storedFade !== null) fadeSel.value = String(fade);
    rtaFade.set(fade);
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
