import { setDomProperty } from './domState';
import { syncSelectValue } from './selectControl';
// Waterfall colour controls. SDR retains measured levels, so range, palette and
// contrast changes can recolour history even while the waterfall is paused.
import { applyI18n, t } from '../core/i18n';
import * as S from '../core/store';
import { requestRender } from '../render/redraw';
import { send } from '../core/wsSend';
import { deviceMode, measurementOwnsDevice } from './graphMode';
import { waterfallOn, wfContrast, wfFloorSource, wfHiDbm, wfLoDbm, wfPalette, wfRangeMode, type WaterfallPalette } from './waterfallState';

const LS_KEY = 'websa-wf-range';
const RANGE_ERROR = 'Waterfall range requires finite levels with Low less than High.';
const confirmedFftSizes = new WeakMap<HTMLSelectElement, number>();

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
  setDomProperty(notice, 'hidden', !message);
}

function load(): void {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (raw === null) return;
    const d = JSON.parse(raw);
    if (d?.floorSource === 'view' || d?.floorSource === 'capture') wfFloorSource.set(d.floorSource);
    if (d?.palette === 'classic' || d?.palette === 'blue' || d?.palette === 'viridis' || d?.palette === 'amber') wfPalette.set(d.palette);
    if (typeof d?.contrast === 'number' && Number.isFinite(d.contrast) && d.contrast >= 0.5 && d.contrast <= 2) wfContrast.set(d.contrast);
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
    localStorage.setItem(LS_KEY, JSON.stringify({ floorSource: wfFloorSource.get(), mode: wfRangeMode.get(), lo: wfLoDbm.get(), hi: wfHiDbm.get(), palette: wfPalette.get(), contrast: wfContrast.get() }));
  } catch { /* storage unavailable */ }
}

export function resetWfRange(): void {
  for (const slot of [wfRangeMode, wfFloorSource, wfPalette, wfContrast, wfLoDbm, wfHiDbm]) slot.reset();
  wfRangeMode.confirm('auto'); wfFloorSource.confirm('capture'); wfPalette.confirm('classic');
  wfContrast.confirm(1); wfLoDbm.confirm(-110); wfHiDbm.confirm(-30);
  try { localStorage.removeItem(LS_KEY); } catch { /* storage unavailable */ }
  validation(''); syncWfRangeUi(); requestRender();
}

/** Graph controls follow the waterfall and acquisition mode independently of RF levels. */
export function syncWfRangeUi(): void {
  const sel = el<HTMLSelectElement>('select-wf-range');
  const disabled = !waterfallOn.get() || measurementOwnsDevice() || deviceMode.get() === 'server';
  const settings = el('waterfall-settings');
  if (settings) setDomProperty(settings, 'hidden', disabled);
  const sourceRow = el('wf-floor-source-row');
  if (sourceRow) setDomProperty(sourceRow, 'hidden', !S.sdrMode);
  if (sel) { syncSelectValue(sel, wfRangeMode.get()); setDomProperty(sel, 'disabled', disabled); }
  const source = el<HTMLSelectElement>('select-wf-floor-source');
  if (source) { syncSelectValue(source, wfFloorSource.get()); setDomProperty(source, 'disabled', disabled || !S.sdrMode || wfRangeMode.get() === 'fixed'); }
  const palette = el<HTMLSelectElement>('select-wf-palette');
  if (palette) { syncSelectValue(palette, wfPalette.get()); setDomProperty(palette, 'disabled', disabled); }
  const contrast = el<HTMLInputElement>('input-wf-contrast');
  if (contrast) { contrast.value = String(wfContrast.get()); setDomProperty(contrast, 'disabled', disabled); }
  const contrastValue = el<HTMLOutputElement>('wf-contrast-value');
  if (contrastValue) contrastValue.value = wfContrast.get().toFixed(2);
  const lo = el<HTMLInputElement>('input-wf-lo');
  const hi = el<HTMLInputElement>('input-wf-hi');
  const fixed = wfRangeMode.get() === 'fixed';
  if (lo) {
    if (document.activeElement !== lo) lo.value = String(wfLoDbm.get());
    setDomProperty(lo, 'disabled', disabled || !fixed);
  }
  if (hi) {
    if (document.activeElement !== hi) hi.value = String(wfHiDbm.get());
    setDomProperty(hi, 'disabled', disabled || !fixed);
  }
}

// SWP/RTA store pre-mapped density; SDR stores raw levels and keeps its history.
function changeMapping(change: () => void): void {
  const mode = wfRangeMode.get(), lo = wfLoDbm.get(), hi = wfHiDbm.get();
  change();
  if (!S.sdrMode && (mode !== wfRangeMode.get() || (wfRangeMode.get() === 'fixed'
    && (lo !== wfLoDbm.get() || hi !== wfHiDbm.get())))) S.resetWaterfall();
  syncWfRangeUi();
  requestRender();
}

export function initWfRange(): void {
  const panel = document.getElementById('graph-panel');
  if (panel) applyI18n(panel);
  changeMapping(load);
  el<HTMLSelectElement>('select-sdr-fft')?.addEventListener('change', (event) => {
    const input = event.target as HTMLSelectElement;
    const value = Number(input.value);
    // Only STATUS can show an applied tier; a request can fail capability checks.
    syncSelectValue(input, String(confirmedFftSizes.get(input) ?? 0), true);
    input.blur();
    if (input.disabled || ![0, 32768, 65536].includes(value)) return;
    send({ cmd: 'SET_SDR_FFT', size: value });
  });
  el<HTMLSelectElement>('select-wf-floor-source')?.addEventListener('change', (event) => {
    const input = event.target as HTMLSelectElement;
    if (input.disabled || !['capture', 'view'].includes(input.value)) return;
    changeMapping(() => wfFloorSource.set(input.value as 'capture' | 'view'));
    save();
  });
  el<HTMLSelectElement>('select-wf-palette')?.addEventListener('change', (event) => {
    const input = event.target as HTMLSelectElement;
    if (input.disabled || !['classic', 'blue', 'viridis', 'amber'].includes(input.value)) return;
    changeMapping(() => wfPalette.set(input.value as WaterfallPalette));
    save();
  });
  el<HTMLInputElement>('input-wf-contrast')?.addEventListener('input', (event) => {
    const input = event.target as HTMLInputElement;
    const value = Number(input.value);
    if (input.disabled || !Number.isFinite(value) || value < 0.5 || value > 2) return;
    changeMapping(() => wfContrast.set(value));
    save();
  });
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

export function syncDisplayFftUi(active: boolean, actual: any, mode = deviceMode.get()): void {
  const select = el<HTMLSelectElement>('select-sdr-fft');
  if (!select) return;
  const row = el('sdr-fft-settings');
  if (row) setDomProperty(row, 'hidden', mode !== 'sdr');
  const size = [32768, 65536].includes(actual?.size) ? actual.size : 0;
  // Keep telemetry out of DOM attributes: even data-* mutations can rebuild a
  // Firefox/Zen native popup and reset its hover without changing select.value.
  confirmedFftSizes.set(select, size);
  // Setting even an unchanged value resets the native popup's hovered option.
  syncSelectValue(select, String(size));
  const disabled = !active || actual?.available !== true;
  setDomProperty(select, 'disabled', disabled);
  const output = el<HTMLOutputElement>('sdr-fft-actual');
  if (output) {
    const text = actual?.state === 'warming' ? t('sdr_fft_pending', { size: actual.requested_size })
      : actual?.error ? t('sdr_fft_fallback')
      : size && Number.isFinite(actual?.observation_ms)
        ? `${actual.observation_ms.toFixed(1)} ms · Δf ${actual.bin_spacing_hz.toFixed(1)} Hz · ENBW ${actual.enbw_hz.toFixed(1)} Hz` : '';
    setDomProperty(output, 'value', text);
    setDomProperty(output, 'title', String(actual?.error ?? ''));
  }
}
