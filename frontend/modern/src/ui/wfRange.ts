// Waterfall colour range control: Auto (per-frame relative) or Fixed (absolute dBm window).
//
// Both spectrum modes build their waterfall row through the same mapping (render/waterfall.ts),
// so this setting applies to SWP and RTA alike.
import * as S from '../core/store';
import { applyI18n, t } from '../core/i18n';
import { renderAll } from '../render/spectrum';

const LS_KEY = 'websa-wf-range';

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function load(): void {
  try {
    const d = JSON.parse(localStorage.getItem(LS_KEY) ?? '');
    if (d?.mode === 'fixed' || d?.mode === 'auto') S.setWfRangeMode(d.mode);
    if (Number.isFinite(Number(d?.lo))) S.setWfLoDbm(Number(d.lo));
    if (Number.isFinite(Number(d?.hi))) S.setWfHiDbm(Number(d.hi));
  } catch { /* defaults */ }
}

function save(): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify({ mode: S.wfRangeMode, lo: S.wfLoDbm, hi: S.wfHiDbm }));
  } catch { /* storage unavailable */ }
}

function syncUi(): void {
  const sel = el<HTMLSelectElement>('select-wf-range');
  if (sel) sel.value = S.wfRangeMode;
  const lo = el<HTMLInputElement>('input-wf-lo');
  const hi = el<HTMLInputElement>('input-wf-hi');
  const fixed = S.wfRangeMode === 'fixed';
  if (lo) { lo.value = String(S.wfLoDbm); lo.disabled = !fixed; }
  if (hi) { hi.value = String(S.wfHiDbm); hi.disabled = !fixed; }
}

export function initWfRange(): void {
  const panel = document.getElementById('graph-panel');
  if (panel) applyI18n(panel);
  load();
  syncUi();
  el<HTMLSelectElement>('select-wf-range')?.addEventListener('change', (e) => {
    S.setWfRangeMode((e.target as HTMLSelectElement).value === 'fixed' ? 'fixed' : 'auto');
    save();
    syncUi();
    renderAll();
  });
  el<HTMLInputElement>('input-wf-lo')?.addEventListener('change', (e) => {
    const v = parseFloat((e.target as HTMLInputElement).value);
    if (Number.isFinite(v)) S.setWfLoDbm(v);
    save();
    syncUi();
    renderAll();
  });
  el<HTMLInputElement>('input-wf-hi')?.addEventListener('change', (e) => {
    const v = parseFloat((e.target as HTMLInputElement).value);
    if (Number.isFinite(v)) S.setWfHiDbm(v);
    save();
    syncUi();
    renderAll();
  });
}
