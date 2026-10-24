import { beforeEach, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { pushSpectrumRow } from '../render/waterfall';
import { initWfRange, syncWfRangeUi } from '../ui/wfRange';
import { wfHiDbm, wfLoDbm, wfPaused, wfRangeMode } from '../ui/waterfallState';

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
  localStorage.removeItem('websa-wf-range');
  S.resetWaterfall();
  document.body.innerHTML = `<div id="level-panel"><select id="select-wf-range"><option value="auto">Auto</option><option value="fixed">Fixed</option></select><label>Low<input id="input-wf-lo" type="number"></label><label>High<input id="input-wf-hi" type="number"></label></div>`;
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

it('keeps mode exclusions when the Level panel becomes available again', () => {
  initWfRange();
  const panel = document.getElementById('level-panel')!;
  panel.setAttribute('aria-disabled', 'true');
  syncWfRangeUi();
  for (const id of ['select-wf-range', 'input-wf-lo', 'input-wf-hi']) {
    expect((document.getElementById(id) as HTMLInputElement).disabled).toBe(true);
  }
  panel.setAttribute('aria-disabled', 'false');
  syncWfRangeUi();
  expect((document.getElementById('select-wf-range') as HTMLSelectElement).disabled).toBe(false);
  expect((document.getElementById('input-wf-lo') as HTMLInputElement).disabled).toBe(true);
  edit('select-wf-range', 'fixed');
  expect((document.getElementById('input-wf-lo') as HTMLInputElement).disabled).toBe(false);
  expect((document.getElementById('input-wf-hi') as HTMLInputElement).disabled).toBe(false);
});
