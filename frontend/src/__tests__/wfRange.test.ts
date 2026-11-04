import { afterEach, beforeEach, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { pushSpectrumRow } from '../render/waterfall';
import { initWfRange, syncWfRangeUi } from '../ui/wfRange';
import { waterfallOn, wfContrast, wfFloorSource, wfHiDbm, wfLoDbm, wfPalette, wfPaused, wfRangeMode } from '../ui/waterfallState';
import { deviceMode } from '../ui/graphMode';

function edit(id: string, value: string): void {
  const input = document.getElementById(id) as HTMLInputElement;
  input.value = value;
  input.dispatchEvent(new Event('change', { bubbles: true }));
}

function push(levels = [-110, -70, -30]): void {
  pushSpectrumRow(new Float32Array(levels), new Float64Array([1, 2, 3]), 1, 3, 100);
}

beforeEach(() => {
  resetAll();
  waterfallOn.set(true);
  localStorage.removeItem('websa-wf-range');
  S.resetWaterfall();
  S.setSdrMode(false);
  document.body.innerHTML = `<div id="graph-panel"><div id="waterfall-settings"><select id="select-wf-range"><option value="auto">Auto</option><option value="fixed">Fixed</option></select><label>Low<input id="input-wf-lo" type="number"></label><label>High<input id="input-wf-hi" type="number"></label><div id="wf-floor-source-row"><select id="select-wf-floor-source"><option value="capture">Full Capture</option><option value="view">Current View</option></select></div><select id="select-wf-palette"><option value="classic">Classic Colour</option><option value="blue">SDR Blue</option><option value="viridis">Viridis</option><option value="amber">Amber</option></select><input type="range" id="input-wf-contrast" min="0.5" max="2" step="0.05"><output id="wf-contrast-value"></output></div></div>`;
});

afterEach(() => S.setSdrMode(false));

it('preserves paused SDR amplitude history when adjusting range, palette and contrast', () => {
  S.setSdrMode(true);
  initWfRange();
  push();
  const row = S.waterfallRows[0];
  const original = row.slice();
  wfPaused.set(true);
  edit('select-wf-range', 'fixed');
  edit('input-wf-lo', '-120');
  edit('input-wf-hi', '-20');
  edit('select-wf-palette', 'amber');
  const contrast = document.getElementById('input-wf-contrast') as HTMLInputElement;
  contrast.value = '0.75'; contrast.dispatchEvent(new Event('input', { bubbles: true }));
  expect(S.waterfallRows).toEqual([row]);
  expect(row).toEqual(original);
  expect(wfPaused.get()).toBe(true);
  expect(document.getElementById('wf-contrast-value')?.textContent).toBe('0.75');
  expect(JSON.parse(localStorage.getItem('websa-wf-range')!)).toEqual({ floorSource: 'capture', mode: 'fixed', lo: -120, hi: -20, palette: 'amber', contrast: 0.75 });
});

it('restores waterfall appearance and rejects invalid saved palette or contrast', () => {
  localStorage.setItem('websa-wf-range', JSON.stringify({ mode: 'auto', lo: -110, hi: -30, palette: 'viridis', contrast: 0.8 }));
  initWfRange();
  expect(wfPalette.get()).toBe('viridis');
  expect(wfContrast.get()).toBe(0.8);
  resetAll('wf');
  localStorage.setItem('websa-wf-range', JSON.stringify({ mode: 'auto', lo: -110, hi: -30, palette: 'unknown', contrast: -1 }));
  initWfRange();
  expect(wfPalette.get()).toBe('classic');
  expect(wfContrast.get()).toBe(1);
});

it('rejects invalid commits, restores visible valid levels and preserves quantized history', () => {
  initWfRange();
  edit('select-wf-range', 'fixed');
  push();
  const row = S.waterfallRows[0];
  for (const value of ['-30', '5', '', 'Infinity']) {
    edit('input-wf-lo', value);
    expect(wfLoDbm.get()).toBe(-110);
    expect((document.getElementById('input-wf-lo') as HTMLInputElement).value).toBe('-110');
    expect(document.getElementById('wf-range-validation')?.textContent).toContain('Low less than High');
    expect(document.getElementById('wf-range-validation')?.hidden).toBe(false);
    expect(S.waterfallRows).toEqual([row]);
  }
  edit('input-wf-hi', '-110');
  expect(wfHiDbm.get()).toBe(-30);
  expect((document.getElementById('input-wf-hi') as HTMLInputElement).value).toBe('-30');
});

it.each([
  { lo: -30, hi: -110 }, { lo: -30, hi: -30 }, { lo: null, hi: -30 },
  { lo: '-110', hi: -30 }, { lo: -110, hi: 'Infinity' },
])('rejects invalid persisted window %j as a pair', (range) => {
  localStorage.setItem('websa-wf-range', JSON.stringify({ mode: 'fixed', ...range }));
  initWfRange();
  expect([wfLoDbm.get(), wfHiDbm.get()]).toEqual([-110, -30]);
  push();
  expect(Array.from(S.waterfallRows[0])).toEqual([0, 50, 100]);
  expect(document.getElementById('wf-range-validation')?.hidden).toBe(false);
});

it('clears old mapping synchronously while paused and uses the exact new fixed window', () => {
  initWfRange();
  push();
  wfPaused.set(true);
  edit('select-wf-range', 'fixed');
  expect(S.waterfallRows).toEqual([]);
  push();
  edit('input-wf-lo', '-30.5');
  expect(S.waterfallRows).toEqual([]);
  push([-30.5, -30.25, -30]);
  expect(Array.from(S.waterfallRows[0])).toEqual([0, 50, 100]);
  edit('input-wf-hi', '-29.5');
  expect(S.waterfallRows).toEqual([]);
  push([-30.5, -30, -29.5]);
  expect(Array.from(S.waterfallRows[0])).toEqual([0, 50, 100]);
  edit('input-wf-hi', '-29.5');
  expect(S.waterfallRows).toHaveLength(1);
  edit('select-wf-range', 'auto');
  expect(S.waterfallRows).toEqual([]);
  expect(wfPaused.get()).toBe(true);
});

it('loads a valid fixed window and maps incoming levels to it', () => {
  localStorage.setItem('websa-wf-range', JSON.stringify({ mode: 'fixed', lo: -80, hi: -40 }));
  initWfRange();
  expect(wfRangeMode.get()).toBe('fixed');
  push([-80, -60, -40]);
  expect(Array.from(S.waterfallRows[0])).toEqual([0, 50, 100]);
});

it('keeps mode exclusions when the Graph controls become available again', () => {
  initWfRange();
  deviceMode.confirm('harmonic');
  syncWfRangeUi();
  for (const id of ['select-wf-range', 'input-wf-lo', 'input-wf-hi']) {
    expect((document.getElementById(id) as HTMLInputElement).disabled).toBe(true);
  }
  deviceMode.confirm('std');
  syncWfRangeUi();
  expect((document.getElementById('select-wf-range') as HTMLSelectElement).disabled).toBe(false);
  expect((document.getElementById('input-wf-lo') as HTMLInputElement).disabled).toBe(true);
  edit('select-wf-range', 'fixed');
  expect((document.getElementById('input-wf-lo') as HTMLInputElement).disabled).toBe(false);
  expect((document.getElementById('input-wf-hi') as HTMLInputElement).disabled).toBe(false);
});

it('follows waterfall visibility and enables the SDR floor only with automatic range', () => {
  initWfRange();
  const settings = document.getElementById('waterfall-settings')!;
  const sourceRow = document.getElementById('wf-floor-source-row')!;
  const source = document.getElementById('select-wf-floor-source') as HTMLSelectElement;
  expect(settings.hidden).toBe(false);
  expect(sourceRow.hidden).toBe(true);
  S.setSdrMode(true); deviceMode.confirm('sdr'); syncWfRangeUi();
  expect(sourceRow.hidden).toBe(false);
  expect(source.disabled).toBe(false);
  edit('select-wf-floor-source', 'view');
  edit('select-wf-range', 'fixed');
  expect(source.disabled).toBe(true);
  expect(wfFloorSource.get()).toBe('view');
  waterfallOn.set(false); syncWfRangeUi();
  expect(settings.hidden).toBe(true);
  expect([...settings.querySelectorAll('input,select')].every(c => (c as HTMLInputElement).disabled)).toBe(true);
  waterfallOn.set(true); syncWfRangeUi();
  expect(settings.hidden).toBe(false);
  expect(source.disabled).toBe(true);
  expect((document.getElementById('input-wf-lo') as HTMLInputElement).disabled).toBe(false);
  deviceMode.confirm('server'); syncWfRangeUi();
  expect(settings.hidden).toBe(true);
});
