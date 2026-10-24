// Waterfall colour range control: Auto (per-frame relative) or Fixed (absolute dBm window).
// Both spectrum modes share this mapping and retain only quantized history.
import { applyI18n } from '../core/i18n';
import { resetWaterfall } from '../core/store';
import { requestRender } from '../render/redraw';
import { wfHiDbm, wfLoDbm, wfRangeMode } from './waterfallState';

const LS_KEY = 'websa-wf-range';
const RANGE_ERROR = 'Waterfall range requires finite levels with Low less than High.';

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function validRange(lo: unknown, hi: unknown): lo is number {
  return typeof lo === 'number' && typeof hi === 'number'
    && Number.isFinite(lo) && Number.isFinite(hi) && lo < hi;
}

function validation(message: string): void {
  let notice = el('wf-range-validation');
  if (!notice) {
    const hi = el('input-wf-hi');
    if (!hi) return;
    notice = document.createElement('span');
    notice.id = 'wf-range-validation';
    notice.setAttribute('role', 'alert');
    hi.parentElement?.append(notice);
    for (const id of ['input-wf-lo', 'input-wf-hi']) {
      el(id)?.setAttribute('aria-describedby', notice.id);
    }
  }
  notice.textContent = message;
  notice.hidden = !message;
}

function load(): void {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (raw === null) return;
    const d = JSON.parse(raw);
    if (d?.mode === 'fixed' || d?.mode === 'auto') wfRangeMode.set(d.mode);
    if (validRange(d?.lo, d?.hi)) {
      wfLoDbm.set(d.lo);
      wfHiDbm.set(d.hi);
    } else {
      validation(RANGE_ERROR);
    }
  } catch { validation(RANGE_ERROR); }
}

function save(): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify({ mode: wfRangeMode.get(), lo: wfLoDbm.get(), hi: wfHiDbm.get() }));
  } catch { /* storage unavailable */ }
}

/** Reapply mode-specific availability after the Level panel's global disable pass. */
export function syncWfRangeUi(): void {
  const sel = el<HTMLSelectElement>('select-wf-range');
  const panel = sel?.closest('#level-panel, [data-level-controls]');
  const disabled = !!panel && (panel.classList.contains('disabled')
    || panel.getAttribute('aria-disabled') === 'true' || panel.hasAttribute('disabled'));
  if (sel) { sel.value = wfRangeMode.get(); sel.disabled = disabled; }
  const lo = el<HTMLInputElement>('input-wf-lo');
  const hi = el<HTMLInputElement>('input-wf-hi');
  const fixed = wfRangeMode.get() === 'fixed';
  if (lo) {
    if (document.activeElement !== lo) lo.value = String(wfLoDbm.get());
    lo.disabled = disabled || !fixed;
  }
  if (hi) {
    if (document.activeElement !== hi) hi.value = String(wfHiDbm.get());
    hi.disabled = disabled || !fixed;
  }
}

// A changed mapping cannot reuse quantized rows: clearing is synchronous, even paused.
function changeMapping(change: () => void): void {
  const mode = wfRangeMode.get(), lo = wfLoDbm.get(), hi = wfHiDbm.get();
  change();
  if (mode !== wfRangeMode.get() || (wfRangeMode.get() === 'fixed'
    && (lo !== wfLoDbm.get() || hi !== wfHiDbm.get()))) resetWaterfall();
  syncWfRangeUi();
  requestRender();
}

export function initWfRange(): void {
  const panel = document.getElementById('graph-panel');
  if (panel) applyI18n(panel);
  changeMapping(load);
  el<HTMLSelectElement>('select-wf-range')?.addEventListener('change', (e) => {
    const input = e.target as HTMLSelectElement;
    if (input.disabled) return;
    changeMapping(() => wfRangeMode.set(input.value === 'fixed' ? 'fixed' : 'auto'));
    save();
    validation('');
  });
  for (const id of ['input-wf-lo', 'input-wf-hi']) {
    el<HTMLInputElement>(id)?.addEventListener('change', (e) => {
      const input = e.target as HTMLInputElement;
      if (input.disabled) return;
      const value = input.value.trim() === '' ? NaN : Number(input.value);
      const lo = id === 'input-wf-lo' ? value : wfLoDbm.get();
      const hi = id === 'input-wf-hi' ? value : wfHiDbm.get();
      if (!validRange(lo, hi)) {
        input.value = String(id === 'input-wf-lo' ? wfLoDbm.get() : wfHiDbm.get());
        syncWfRangeUi();
        validation(RANGE_ERROR);
        return;
      }
      changeMapping(() => {
        wfLoDbm.set(lo);
        wfHiDbm.set(hi);
      });
      save();
      validation('');
    });
  }
}
