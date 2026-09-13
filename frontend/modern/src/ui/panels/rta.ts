// RTA panel: analysis window, span stepping and the density background.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { normalizeCenterSpan } from '../../core/frequency';
import { beginFrequencyCommit, validateFrequencyWindow } from './frequency';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';

export function clearRtaAccum() {
  // A reconfiguration (span/rbw/sweep) invalidates every accumulation on the old
  // frequency axis / resolution: probability density, per-trace displays, waterfall.
  if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
  for (let ti = 0; ti < S.rtaDisplays.length; ti++) S.rtaDisplays[ti] = null;
  for (let ti = 0; ti < S.rtaAvgN.length; ti++) S.rtaAvgN[ti] = 0;
  S.resetWaterfall();
}

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
