// RTA panel: analysis window, span stepping and the density background.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { normalizeCenterSpan } from '../../core/frequency';
import { beginFrequencyCommit, validateFrequencyWindow } from './frequency';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { rtaAmpBins, rtaFade } from '../waterfallState';
import { units } from '../../core/units';

export function clearRtaAccum() {
  // A reconfiguration (span/rbw/sweep) invalidates every accumulation on the old
  // frequency axis / resolution: probability density, per-trace displays, waterfall.
  if (S.rtaDensity2d) S.rtaDensity2d!.fill(0);
  for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
  for (let ti = 0; ti < S.rtaAvgN.length; ti++) S.rtaAvgN[ti] = 0;
  S.resetWaterfall();
}

// The four persistence gears, plus Off (0), which switches the density layer off entirely
// (see dsp/rtaDensity.ts).
export const RTA_FADE_OFF = 0;
/** The panel's own default gear, used when a value cannot be read at all. */
const RTA_FADE_DEFAULT = 0.975;

/**
 * Density persistence, as a user setting: applied to the slot, the form and the browser storage.
 *
 * Off (0) has to survive the round trip. The handler this replaces wrote `parseFloat(v) || 0.98`,
 * which quietly turned Off back into "about Medium" on the way in - the option would have looked
 * selectable and done nothing.
 */
export function setRtaFade(value: number) {
  const fade = isFinite(value) ? value : RTA_FADE_DEFAULT;
  rtaFade.set(fade);
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

// Density grain: changing bins invalidates the current density array (ws.ts rebuilds it
// automatically on the next frame because the length no longer matches).
export function setRtaBins(bins: number) {
  rtaAmpBins.set(bins);
  if (S.rtaDensity2d) S.rtaDensity2d!.fill(0);
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
  beginFrequencyCommit('rta-freq-settings');
  send({ cmd: 'SET_RTA', center: window!.center, span: window!.span });
}
