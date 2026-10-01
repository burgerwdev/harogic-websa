/**
 * Normalisation status indicator (leaf).
 *
 * It only reads the trace state and toggles one button, but it used to live in
 * dsp/normalize.ts while dsp/traces.ts called it - a two-module cycle
 * (docs/.../ARCH_REVIEW.md finding P1-4). Keeping it in a leaf lets both sides use it.
 */
import * as S from '../core/store';

export function updateNormalizeStatusUI() {
  const t = S.traces[S.activeTraceIdx];
  const btn = document.getElementById('btn-normalize');
  if (btn) btn.classList.toggle('active', !!(t.reference && t.isNormalized));
}
