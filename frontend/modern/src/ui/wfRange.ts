// Waterfall colour range control: Auto (per-frame relative) or Fixed (absolute dBm window).
//
// Both spectrum modes build their waterfall row through the same mapping (render/waterfall.ts),
// so this setting applies to SWP and RTA alike.
import { applyI18n } from '../core/i18n';
import { requestRender } from '../render/redraw';
import { wfHiDbm, wfLoDbm, wfRangeMode } from './waterfallState';

const LS_KEY = 'websa-wf-range';

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function load(): void {
  try {
    const d = JSON.parse(localStorage.getItem(LS_KEY) ?? '');
    if (d?.mode === 'fixed' || d?.mode === 'auto') wfRangeMode.set(d.mode);
    if (Number.isFinite(Number(d?.lo))) wfLoDbm.set(Number(d.lo));
    if (Number.isFinite(Number(d?.hi))) wfHiDbm.set(Number(d.hi));
  } catch { /* defaults */ }
}

function save(): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify({ mode: wfRangeMode.get(), lo: wfLoDbm.get(), hi: wfHiDbm.get() }));
  } catch { /* storage unavailable */ }
}

function syncUi(): void {
  const sel = el<HTMLSelectElement>('select-wf-range');
  if (sel) sel.value = wfRangeMode.get();
  const lo = el<HTMLInputElement>('input-wf-lo');
  const hi = el<HTMLInputElement>('input-wf-hi');
  const fixed = wfRangeMode.get() === 'fixed';
  if (lo) { lo.value = String(wfLoDbm.get()); lo.disabled = !fixed; }
  if (hi) { hi.value = String(wfHiDbm.get()); hi.disabled = !fixed; }
}

export function initWfRange(): void {
  const panel = document.getElementById('graph-panel');
  if (panel) applyI18n(panel);
  load();
  syncUi();
  el<HTMLSelectElement>('select-wf-range')?.addEventListener('change', (e) => {
    wfRangeMode.set((e.target as HTMLSelectElement).value === 'fixed' ? 'fixed' : 'auto');
    save();
    syncUi();
    requestRender();
  });
  el<HTMLInputElement>('input-wf-lo')?.addEventListener('change', (e) => {
    const v = parseFloat((e.target as HTMLInputElement).value);
    if (Number.isFinite(v)) wfLoDbm.set(v);
    save();
    syncUi();
    requestRender();
  });
  el<HTMLInputElement>('input-wf-hi')?.addEventListener('change', (e) => {
    const v = parseFloat((e.target as HTMLInputElement).value);
    if (Number.isFinite(v)) wfHiDbm.set(v);
    save();
    syncUi();
    requestRender();
  });
}
