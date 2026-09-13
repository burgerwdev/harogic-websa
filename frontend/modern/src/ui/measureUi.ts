/**
 * Measurement-panel visibility (leaf).
 *
 * `ui/measure.ts` owns the measurement state machine, but the harmonic and phase-noise
 * modules need to refresh the panel after a measurement completes. Importing that back
 * from measure.ts created a cycle; the DOM-only helpers live here instead
 * (docs/.../ARCH_REVIEW.md finding P1-4).
 */
import * as S from '../core/store';
import { syncChanTableVisibility } from '../meas/channel';

export function setMeasButtons(en: boolean) {
  ['btn-amp-meas', 'btn-amp-clear', 'btn-harm-set', 'btn-harm-mode',
    'btn-pnm-set', 'btn-pnm-apply', 'btn-chan-meas', 'btn-chan-clear'].forEach(id => {
    const b = document.getElementById(id) as HTMLButtonElement;
    if (b) b.disabled = !en;
  });
  // modern buttons may have different ids; also support the data-action approach
  const acts = ['meas-amp', 'clear-amp', 'meas-harm', 'meas-pnm', 'meas-chan', 'clear-chan'];
  acts.forEach(a => {
    const el = document.querySelector(`[data-action="${a}"]`) as HTMLButtonElement;
    if (el) el.disabled = !en;
  });
}

export function applyMeasUI() {
  const inMeas = S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm');
  document.body.classList.toggle('meas-mode', inMeas);
  const chanTab = S.measOn && S.measTabSel === 'chan';
  const mt = document.getElementById('marker-table');
  if (mt) mt.style.display = ((S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm')) || chanTab) ? 'none' : '';
  syncChanTableVisibility();
  const ht = document.getElementById('harmonic-table');
  if (ht) ht.style.display = (S.measOn && S.viewMode === 'harm') ? '' : 'none';
  const pt = document.getElementById('pnm-table');
  if (pt) pt.style.display = (S.measOn && S.viewMode === 'pnm') ? '' : 'none';
  if ((S.viewMode === 'harm' || S.viewMode === 'pnm') && !S.stdSnap) {
    S.setStdSnap({ traces: S.traces.map(t => ({ mode: t.mode })), markers: S.markers.map(m => Object.assign({}, m)) });
  }
}
