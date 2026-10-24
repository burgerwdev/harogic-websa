/**
 * Normalisation status indicator (leaf).
 *
 * Shared by normalization and trace updates; reads trace state and updates the button.
 */
import * as S from '../core/store';

export function updateNormalizeStatusUI() {
  const t = S.traces[S.activeTraceIdx];
  const btn = document.getElementById('btn-normalize');
  if (btn) btn.classList.toggle('active', !!(t.reference && t.isNormalized));
}
