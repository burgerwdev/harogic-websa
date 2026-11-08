import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { patchWorkbench } from '../ui/workbenchDom';
import { syncSelectOptions, syncSelectValue } from '../ui/selectControl';
import { mountQuickListen } from '../ui/quickListen';
import { WorkbenchSession } from '../ui/workbenchSession';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { updateMarkerTable } from '../render/markerTable';
import * as S from '../core/store';

beforeEach(() => { document.body.replaceChildren(); });
afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

it('leaves marker mode and reference menus untouched during spectrum redraws', () => {
  document.body.innerHTML = '<table><tbody id="marker-tbody"></tbody></table>';
  const saved = S.markers.map(marker => ({ ...marker }));
  try {
    S.markers[0].enabled = true; S.markers[0].mode = 'NORMAL';
    updateMarkerTable(null);
    const selects = [...document.querySelectorAll('select')];
    expect(selects).toHaveLength(S.markers.length * 2);
    const observer = new MutationObserver(() => {});
    for (const select of selects) observer.observe(select, { attributes: true, subtree: true, childList: true, characterData: true });
    const setters = selects.map(select => vi.spyOn(select, 'value', 'set'));
    for (const select of selects) {
      select.focus();
      for (let i = 0; i < 10; i++) updateMarkerTable(new Float32Array([-20 - i, -40]));
      select.blur();
    }
    expect(observer.takeRecords()).toEqual([]); observer.disconnect();
    for (const setter of setters) expect(setter).not.toHaveBeenCalled();
  } finally { S.markers.forEach((marker, index) => Object.assign(marker, saved[index])); }
});

it.each([false, true].flatMap(other => [false, true].map(optionFocus => ({ other, optionFocus }))))(
  'preserves an active menu across status changes (other document: $other, option focus: $optionFocus)', ({ other, optionFocus }) => {
  const frame = document.createElement('iframe'); document.body.append(frame);
  const doc = other ? frame.contentDocument! : document;
  const host = doc.createElement('div'); doc.body.append(host);
  const markup = (updated: boolean) => `<select name="channel">${updated ? '<option value="new">New</option>' : ''}<option value="a">${updated ? 'Renamed' : 'A'}</option><option value="b" ${updated ? 'selected' : ''}>B</option></select>`;
  patchWorkbench(host, markup(false));
  const select = host.querySelector('select')!, options = [...select.options];
  if (optionFocus) { select.options[0].tabIndex = -1; select.options[0].focus(); }
  else select.focus();
  const setter = vi.spyOn(select, 'value', 'set');
  const observer = new MutationObserver(() => {}); observer.observe(select, { childList: true, subtree: true, attributes: true, characterData: true });
  for (let i = 0; i < 5; i++) {
    patchWorkbench(host, markup(true)); syncSelectValue(select, 'b');
    syncSelectOptions(select, [{ value: 'a', label: 'Renamed' }, { value: 'b', label: 'B' }], 'b');
  }
  expect(host.querySelector('select')).toBe(select);
  expect([...select.options]).toEqual(options);
  expect(select.value).toBe('a'); expect(setter).not.toHaveBeenCalled();
  expect(observer.takeRecords()).toEqual([]); observer.disconnect();
  (doc.activeElement as HTMLElement).blur(); patchWorkbench(host, markup(true));
  expect(select.value).toBe('b'); expect(select.options).toHaveLength(3);
});

it('keeps unchanged dynamic choices intact and applies changed choices after leaving the menu', () => {
  const select = document.createElement('select'); document.body.append(select);
  const choices = [{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }];
  syncSelectOptions(select, choices, 'b');
  const options = [...select.options], setter = vi.spyOn(select, 'value', 'set');
  const observer = new MutationObserver(() => {}); observer.observe(select, { subtree: true, attributes: true, childList: true });
  for (let i = 0; i < 5; i++) syncSelectOptions(select, choices, 'b');
  expect([...select.options]).toEqual(options); expect(setter).not.toHaveBeenCalled();
  expect(observer.takeRecords()).toEqual([]);
  select.focus(); syncSelectOptions(select, [choices[0]], 'a');
  expect(select.value).toBe('b'); expect(select.options).toHaveLength(2);
  select.blur(); syncSelectOptions(select, [choices[0]], 'a');
  expect(select.value).toBe('a'); expect(select.options[0]).toBe(options[0]); observer.disconnect();
});

it('does not rebuild quick-listen options or reset a pending selection on periodic snapshots', () => {
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.open = true; } });
  const adapter = new FixtureReceiverAdapter(8, 10), session = new WorkbenchSession(adapter);
  const ui = mountQuickListen(session, document.body);
  try {
    ui.open(7_100_000);
    for (const name of ['mode', 'target']) {
      const select = document.querySelector<HTMLSelectElement>(`[data-quick-listen] select[name=${name}]`)!;
      const options = [...select.options], setter = vi.spyOn(select, 'value', 'set');
      select.focus();
      const observer = new MutationObserver(() => {}); observer.observe(select, { subtree: true, attributes: true, childList: true });
      for (let i = 0; i < 5; i++) session.notify();
      expect([...select.options]).toEqual(options); expect(setter).not.toHaveBeenCalled();
      expect(observer.takeRecords()).toEqual([]); observer.disconnect(); select.blur();
    }
  } finally { ui.dispose(); session.dispose(); adapter.dispose(); }
});
