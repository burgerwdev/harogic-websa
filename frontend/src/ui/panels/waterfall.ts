// Waterfall controls and the sweep-speed field.
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { getUiScale } from '../../core/uiScale';
import { waterfallOn, wfPaused } from '../waterfallState';
import { pushSpectrumRow } from '../../render/waterfall';
import { createParam } from '../../core/params';

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

/** Missing stored height uses the default; zero coercion must not hide the panel. */
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
    // Height is assigned to a border-box container; clientHeight excludes its borders and
    // would move the grab point by another two CSS pixels on every new drag.
    startH = container.offsetHeight || wfHeight;
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
    // Convert the pointer's viewport delta to local CSS pixels using UI scale.
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

export function setWfPaused(paused: boolean): void {
  wfPaused.set(paused);
  const b = document.getElementById('btn-wf-pause');
  if (b) b.classList.toggle('active', paused);
}

export function toggleWfPause() {
  setWfPaused(!wfPaused.get());
}

let lastSwpWfAt = -Infinity;

/** Sample incoming sweeps; redraws never add time to the waterfall history. */
export function sampleSwpWaterfall(powers: Float32Array, freq: Float64Array | null): void {
  if (S.rtaMode || !waterfallOn.get() || wfPaused.get() || !freq || freq.length !== powers.length || freq.length < 2) return;
  const now = performance.now();
  if (now - lastSwpWfAt < 100) return;
  lastSwpWfAt = now;
  pushSpectrumRow(powers, freq, freq[0], freq[freq.length - 1], 100);
}

export function resetWf() {
  S.resetWaterfall();
  requestRender();
}

interface SweepValue { mode: number; time: number }
const sweep = createParam<SweepValue>('swp.sweep', {
  fallback: { mode: 0, time: 0 }, scope: 'swp', ttlMs: Infinity,
  equals: (a, b) => a.mode === b.mode && a.time === b.time,
});
let sweepRequest: string | null = null;
let sweepSequence = 0;
let sweepRetiredThrough = 0;
let sweepConfirmedSequence = 0;
const sweepPrefix = `sweep-${Date.now()}-${Math.random().toString(36).slice(2)}-`;
let sweepMode = '';

function sweepElements() {
  return [document.getElementById('select-sweep-mode'), document.getElementById('input-sweep-time')] as [HTMLSelectElement | null, HTMLInputElement | null];
}

function fillSweepEditor() {
  const [select, input] = sweepElements();
  // Mode, time and visibility form one editor; never mix a report with a draft.
  if (!sweepElements().some(el => el && (document.activeElement === el || el.dataset.edited === '1'))) {
    const value = sweep.get();
    if (select) select.value = String(value.mode);
    if (input) input.value = String(value.time);
  }
  syncSweepInput();
}

export function resetSweepEditor() {
  sweepRequest = null;
  sweepRetiredThrough = sweepSequence;
  sweep.reset();
  for (const el of sweepElements()) if (el) delete el.dataset.edited;
}

export function syncSweepStatus(status: {
  mode: string; response_to?: string; request_id?: string | number;
  sweep_time_mode?: number; sweep_time?: number;
  req?: { swp?: { sweep_time_mode?: number; sweep_time?: number }; rta?: { sweep_time_mode?: number; sweep_time?: number } };
}) {
  if (sweepMode !== status.mode) {
    sweepRequest = null;
    sweepRetiredThrough = sweepSequence;
    sweep.reset();
    sweepMode = status.mode;
  }
  if (status.mode === 'sdr') return;
  const ownSequence = typeof status.request_id === 'string' && status.request_id.startsWith(sweepPrefix)
    ? Number(status.request_id.slice(sweepPrefix.length)) : null;
  const superseded = sweepRequest !== null && ownSequence !== null && status.request_id !== sweepRequest
    && ownSequence > sweepRetiredThrough && ownSequence > sweepConfirmedSequence && ownSequence < sweepSequence;
  if (ownSequence !== null && status.request_id !== sweepRequest && !superseded) return;
  if (sweepRequest !== null && status.request_id !== sweepRequest && !superseded) return;
  const profile = status.req?.[status.mode === 'rta' ? 'rta' : 'swp'];
  const mode = profile?.sweep_time_mode ?? status.sweep_time_mode;
  const time = profile?.sweep_time ?? status.sweep_time;
  if (!Number.isInteger(mode) || mode! < 0 || mode! > 8 || !Number.isFinite(time)) return;
  if (superseded) {
    const desired = sweep.desiredValue();
    sweep.confirm({ mode: mode!, time: time! });
    // Equality may clear desired in Param.confirm; the newer request still owns intent.
    if (desired) sweep.set(desired);
    sweepConfirmedSequence = ownSequence!;
    fillSweepEditor();
    return;
  }
  if (ownSequence !== null) sweepConfirmedSequence = ownSequence;
  sweepRetiredThrough = sweepSequence;
  sweepRequest = null;
  sweep.reset();
  sweep.confirm({ mode: mode!, time: time! });
  fillSweepEditor();
}

export function rejectSweepRequest(requestId: unknown) {
  if (sweepRequest === null || requestId !== sweepRequest) return;
  sweepRequest = null;
  sweepRetiredThrough = sweepSequence;
  sweep.reset();
  fillSweepEditor();
}

export function bindSweepEditor() {
  const [select, input] = sweepElements();
  for (const el of [select, input]) {
    if (!el || el.dataset.sweepBound === '1') continue;
    el.dataset.sweepBound = '1';
    el.addEventListener('input', () => { el.dataset.edited = '1'; syncSweepInput(); });
    el.addEventListener('blur', fillSweepEditor);
  }
  input?.addEventListener('keydown', event => {
    if (event.key === 'Enter') { event.preventDefault(); setSweepSpeed(); }
  });
  fillSweepEditor();
}

export function setSweepSpeed() {
  if (S.sdrMode) return;
  const [select, input] = sweepElements();
  const mode = Number(select?.value ?? 0);
  if (!Number.isInteger(mode) || mode < 0 || mode > 8) return;
  const raw = Number(input?.value ?? 0);
  const minimum = mode === 7 ? 0.001 : 1;
  const time = mode >= 6 ? Math.max(minimum, Number.isFinite(raw) ? raw : minimum) : 0;
  sweepMode = S.rtaMode ? 'rta' : 'std';
  sweepRequest = `${sweepPrefix}${++sweepSequence}`;
  sweep.set({ mode, time });
  for (const el of [select, input]) if (el) delete el.dataset.edited;
  syncSweepInput();
  send({ cmd: 'SET_SWEEP', mode, ...(mode >= 6 ? { time } : {}), request_id: sweepRequest });
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
