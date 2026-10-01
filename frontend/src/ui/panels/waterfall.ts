// Waterfall controls and the sweep-speed field.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { getUiScale } from '../../core/uiScale';
import { waterfallOn, wfPaused } from '../waterfallState';

/// The waterfall's default height: the CSS box the panel always had.
const WF_DEFAULT_H = 135;
/// Below this the waterfall shows a couple of rows and stops being worth dragging; the spectrum
/// keeps at least this much of the column, so the divider can never squeeze the plot away.
const WF_MIN_H = 60;
const SPECTRUM_FLOOR_H = 200;
/// The height the operator last dragged (persisted; a reload restores the split).
const WF_SPLIT_KEY = 'web-sa-wf-split';

/** Clamp a wanted waterfall height into what the column can give it. Pure, so the ends (the
 * spectrum's floor and its own minimum) are testable without a layout. */
export function clampWfHeight(desired: number, columnH: number): number {
  const upper = columnH > 0
    ? Math.max(WF_MIN_H, Math.min(columnH - SPECTRUM_FLOOR_H, columnH * 0.6))
    : WF_DEFAULT_H;
  return Math.round(Math.min(Math.max(WF_MIN_H, desired), upper));
}

/** A stored split, or the default. `Number(null)` is 0, and a stored *absence* must not become a
 * zero-height waterfall (it did, once: the panel came up invisible until the first drag). */
export function parseWfHeight(raw: string | null): number {
  if (raw === null || raw === '') return WF_DEFAULT_H;
  const value = Number(raw);
  return Number.isFinite(value) ? value : WF_DEFAULT_H;
}

function readWfHeight(): number {
  try {
    return parseWfHeight(localStorage.getItem(WF_SPLIT_KEY));
  } catch {
    return WF_DEFAULT_H;
  }
}

let wfHeight = readWfHeight();

/** Apply the remembered split: the container's height and the divider's visibility. Closing the
 * waterfall hides both, which is what returns the area to the spectrum. */
export function syncWfSplit(): void {
  const container = document.getElementById('waterfall-container');
  const split = document.getElementById('wf-split');
  if (container) container.style.height = `${wfHeight}px`;
  if (split) split.style.display = waterfallOn.get() ? '' : 'none';
}

/** Drag the divider: the waterfall grows down, the spectrum gives up the same height (and the
 * spectrum canvas re-derives itself through the store's ResizeObserver). */
export function initWfSplit(): void {
  const split = document.getElementById('wf-split');
  const container = document.getElementById('waterfall-container');
  if (!split || !container) return;
  let dragging = false;
  let startY = 0;
  let startH = 0;
  let columnH = 0;
  split.addEventListener('pointerdown', (event) => {
    const area = document.querySelector('.spectrum-area') as HTMLElement | null;
    dragging = true;
    startY = event.clientY;
    startH = container.clientHeight || wfHeight;
    columnH = area?.clientHeight ?? 0;
    split.classList.add('dragging');
    try {
      split.setPointerCapture(event.pointerId);
    } catch {
      /* not capturable (a synthetic event) */
    }
    event.preventDefault();
  });
  split.addEventListener('pointermove', (event) => {
    if (!dragging) return;
    // The divider must follow the pointer. It sits *below* the spectrum, and its own position is
    // the spectrum's bottom edge - so growing the waterfall (dy > 0) pushes the divider up, away
    // from the cursor. Inverting the delta is what makes the grab point stay under the mouse:
    // dragging down moves the boundary down, the spectrum grows and the waterfall shrinks.
    // The pointer moves in viewport pixels while `height` is in the zoomed frame's local pixels
    // (`.analyzer-card` carries `zoom: var(--ui)`), so the delta is converted before it is applied
    // - without this a 50 px drag moved the divider 65 px at scale 1.25 (measured: the handle
    // outran the cursor by exactly the zoom factor).
    wfHeight = clampWfHeight(startH - (event.clientY - startY) / getUiScale(), columnH);
    container.style.height = `${wfHeight}px`;
  });
  const stop = (event: PointerEvent) => {
    if (!dragging) return;
    dragging = false;
    split.classList.remove('dragging');
    try {
      localStorage.setItem(WF_SPLIT_KEY, String(wfHeight));
    } catch {
      /* storage disabled: the split still holds for this session */
    }
    try {
      split.releasePointerCapture(event.pointerId);
    } catch {
      /* never captured */
    }
  };
  split.addEventListener('pointerup', stop);
  split.addEventListener('pointercancel', stop);
  syncWfSplit();
}

/** Preset: drop the dragged split (and its stored value) so the panels come up at the factory
 * split, like every other panel preference a Preset resets. */
export function resetWfSplit(): void {
  wfHeight = WF_DEFAULT_H;
  try {
    localStorage.removeItem(WF_SPLIT_KEY);
  } catch {
    /* storage disabled: the in-memory default still applies */
  }
  syncWfSplit();
}

/** Turn the waterfall on/off (the measurement panel forces it off while measuring). */
export function setWaterfall(on: boolean) {
  if (waterfallOn.get() === on) return;
  waterfallOn.set(on);
  if (on) S.resetWaterfall();
  const wf = document.getElementById('waterfall');
  if (wf) wf.style.display = on ? '' : 'none';
  const mt = document.getElementById('marker-table');
  if (mt) mt.style.display = on ? 'none' : '';
  const btn = document.getElementById('btn-waterfall');
  if (btn) btn.classList.toggle('active', on);   // text stays "Waterfall", active = on
  syncWfSplit();                                 // the divider follows the waterfall
  requestRender();
}

export function toggleWaterfall() {
  setWaterfall(!waterfallOn.get());
}

export function toggleWfPause() {
  wfPaused.set(!wfPaused.get());
  const b = document.getElementById('btn-wf-pause');
  if (b) b.classList.toggle('active', wfPaused.get());
}

export function resetWf() {
  S.resetWaterfall();
  requestRender();
}

export function setSweepSpeed() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const mode = parseInt(sel?.value || '0');
  const time = parseFloat(tin?.value || '0');
  const m: any = { cmd: 'SET_SWEEP', mode };
  if (mode === 6 || mode === 7 || mode === 8) {
    const minimum = mode === 7 ? 0.001 : 1;
    m.time = Math.max(minimum, isFinite(time) ? time : minimum);
  }
  send(m);
}

export function syncSweepInput() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const btn = document.querySelector('button[data-action="set-sweep"]') as HTMLElement;
  if (!sel) return;
  const mode = parseInt(sel.value || '0');
  const need = mode >= 6;   // minSWTxN(6)/Manual(7)/minSMPxN(8) need an input value
  if (tin) { tin.style.display = need ? '' : 'none'; tin.placeholder = mode === 7 ? t('swt_sec') : t('swt_xn'); }
  if (btn) btn.style.display = need ? '' : 'none';
}
