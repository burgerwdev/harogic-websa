import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { mountReceiverTicker, stripMessages, type ReceiverTicker } from '../ui/receiverTicker';
import type { DecoderLogEntry } from '../sdr/decoderLog';
import { setLang } from '../core/i18n';

let adapter: FixtureReceiverAdapter, ticker: ReceiverTicker, root: HTMLElement, host: HTMLElement, width: number;
beforeEach(() => {
  vi.useFakeTimers(); setLang('en'); adapter = new FixtureReceiverAdapter(); width = 160;
  vi.spyOn(window, 'matchMedia').mockReturnValue({ matches: false } as MediaQueryList);
  root = document.createElement('section'); root.className = 'mc-collapsed'; host = document.createElement('div'); root.append(host); document.body.append(root);
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function(this: HTMLElement) { return this.classList.contains('mc-ticker-viewport') ? width : 0; });
  vi.spyOn(HTMLElement.prototype, 'scrollWidth', 'get').mockImplementation(function(this: HTMLElement) { return (this.textContent?.length ?? 0) * 8; });
  ticker = mountReceiverTicker(host, root);
});
afterEach(() => { ticker.dispose(); adapter.dispose(); document.body.replaceChildren(); vi.restoreAllMocks(); vi.useRealTimers(); });
const entry = (id: number, mode: string, data: unknown, kind: DecoderLogEntry['kind'] = 'text'): DecoderLogEntry => {
  const stream = adapter.snapshot().receivers[0].stream!;
  return { id, mode, source: '99:101', at: Date.now(), kind, data, provider: 'native', bytes: 100,
    timing: { input_index: '0', input_rate: 48000, fraction_num: '0', fraction_den: '1', delay_seconds: 0, delay_known: true, causal_presentation: false, baseband_stream: stream } };
};
const receiver = (mode: string) => { const r = adapter.snapshot().receivers[0]; r.confirmed.processors[0].mode = mode; return r; };
const displayed = () => host.querySelector<HTMLElement>('.mc-ticker-text')!;
it('extracts full text and table messages without a fixed character truncation or mixing streams', () => {
  const long = 'CQ '.repeat(400);
  expect(stripMessages([entry(1, 'ft8', { decodes: [{ msg: long }] }, 'table'), entry(2, 'cw', 'wrong mode'),
    { ...entry(3, 'ft8', 'old stream'), timing: undefined }], receiver('ft8'))).toEqual([{ key: '1:0', text: long }]);
});
it('keeps FT8 location with its message and refreshes country names on language changes', () => {
  const r = receiver('ft8');
  const entries = [entry(1, 'ft8', { decodes: [{ msg: 'CQ W1ABC FN42' }] }, 'table')];
  ticker.update(entries, r, 'a');
  expect(displayed().textContent).toBe('CQ W1ABC FN42 · United States · 42.5°N 71.0°W');
  expect(host.querySelector('.mc-ticker-viewport')?.getAttribute('title')).toContain('42.5°N 71.0°W');
  setLang('zh'); ticker.update(entries, r, 'a');
  expect(displayed().textContent).toBe('CQ W1ABC FN42 · 美国 · 42.5°N 71.0°W');
  ticker.update([entry(2, 'ft8', { decodes: [{ msg: 'W1ABC JA1ABC 73' }] }, 'table')], r, 'a');
  expect(displayed().textContent).toBe('W1ABC JA1ABC 73 · 日本 → 美国');
});
it('does not replay retained text from an old processing stream after recovery', () => {
  const r = receiver('cw'), old = entry(1, 'cw', 'OLD CQ');
  ticker.update([old], r, 'a', '99:101'); expect(displayed().textContent).toBe('OLD CQ');
  ticker.update([old], r, 'a', null); expect(displayed().textContent).toBe('Waiting for decoded text…');
  ticker.update([{ ...entry(2, 'cw', 'NEW CQ'), source: '100:102' }, old], r, 'a', '100:102');
  expect(displayed().textContent).toBe('NEW CQ');
});
it('pauses overview tickers inside hidden windows and after detachment', async () => {
  root.className = 'mc-overview-card';
  const container = document.createElement('section'); document.body.append(container); container.append(root);
  ticker.update([entry(1, 'ft8', { decodes: [{ msg: 'CQ W1ABC FN42 VERY LONG MESSAGE' }] }, 'table')], receiver('ft8'), 'a');
  await vi.advanceTimersByTimeAsync(2200);
  const position = displayed().style.transform;
  container.hidden = true; await vi.advanceTimersByTimeAsync(1200);
  expect(displayed().style.transform).toBe(position);
  container.hidden = false; root.remove(); await vi.advanceTimersByTimeAsync(1200);
  expect(displayed().style.transform).toBe(position);
});
it('scrolls overflowing FT8 text according to available pixels and pauses on hover', async () => {
  const r = receiver('ft8'), entries = [entry(1, 'ft8', { decodes: [{ msg: 'CQ W1ABC FN42 VERY LONG MESSAGE' }] }, 'table')];
  ticker.update(entries, r, 'a'); await vi.advanceTimersByTimeAsync(2200);
  expect(displayed().style.transform).not.toBe('translateX(0px)');
  host.querySelector('.mc-ticker-viewport')!.dispatchEvent(new Event('pointerenter'));
  const position = displayed().style.transform; await vi.advanceTimersByTimeAsync(1500);
  expect(displayed().style.transform).toBe(position);
  host.querySelector('.mc-ticker-viewport')!.dispatchEvent(new Event('pointerleave'));
  width = 1000; await vi.advanceTimersByTimeAsync(80);
  expect(displayed().style.transform).toBe('translateX(0px)');
  ticker.update([entry(2, 'ft8', { decodes: [{ msg: 'NEW CQ' }] }, 'table'), ...entries], r, 'a');
  await vi.advanceTimersByTimeAsync(3200); expect(displayed().textContent).toBe('NEW CQ');
});
it('types only newly arriving CW text, follows its tail, and clears animation state on rebind', async () => {
  const r = receiver('cw'), first = entry(1, 'cw', 'CQ ');
  ticker.update([first], r, 'a'); await vi.advanceTimersByTimeAsync(80);
  ticker.update([entry(2, 'cw', 'DE TEST 😊👍🏽 K'), first], r, 'a');
  await vi.advanceTimersByTimeAsync(40); expect(displayed().textContent).toBe('CQ D');
  await vi.advanceTimersByTimeAsync(1000); expect(displayed().textContent).toBe('CQ DE TEST 😊👍🏽 K');
  ticker.update([], { ...r, receiver_uid: 'another' }, 'a'); await vi.advanceTimersByTimeAsync(300);
  expect(displayed().textContent).toBe('Waiting for decoded text…');
  ticker.update([first], { ...r, state: 'stopped' }, 'a'); expect(displayed().textContent).toBe('Reception paused');
  ticker.dispose(); expect(vi.getTimerCount()).toBe(0);
});
it('freezes when expanded or hidden and resumes without replaying hidden animation time', async () => {
  const r = receiver('cw'), first = entry(1, 'cw', 'CQ ');
  ticker.update([first], r, 'a'); ticker.update([entry(2, 'cw', 'HELLO'), first], r, 'a');
  root.hidden = true; await vi.advanceTimersByTimeAsync(3000); expect(displayed().textContent).toBe('CQ ');
  root.hidden = false; await vi.advanceTimersByTimeAsync(40); expect(displayed().textContent).toBe('CQ H');
});
it('retains decoded text during an out-of-band pause and waits for fresh results after resuming', async () => {
  const r = receiver('cw'), entries = [entry(1, 'cw', 'RETAINED CQ')];
  ticker.update(entries, r, 'a');
  ticker.update(entries, { ...r, coverage: { state: 'out_of_band' }, state: 'interrupted', stream: null }, 'a', null);
  expect(displayed().textContent).toBe('RETAINED CQ');
  const position = displayed().style.transform;
  await vi.advanceTimersByTimeAsync(5000);
  expect(displayed().textContent).toBe('RETAINED CQ'); expect(displayed().style.transform).toBe(position);
  ticker.update(entries, { ...r, coverage: { state: 'in_band' }, stream: { ...r.stream!, epoch: '99' } }, 'a');
  expect(displayed().textContent).toBe('Waiting for decoded text…');
});
it('respects reduced motion while retaining waiting states and the latest full message', async () => {
  vi.mocked(window.matchMedia).mockReturnValue({ matches: true } as MediaQueryList);
  const r = receiver('ft8');
  ticker.update([], r, 'a'); await vi.advanceTimersByTimeAsync(100);
  expect(displayed().textContent).toBe('Waiting for decoded text…');
  ticker.update([entry(2, 'ft8', 'NEW REPORT'), entry(1, 'ft8', 'OLD REPORT')], r, 'a');
  await vi.advanceTimersByTimeAsync(4000);
  expect(displayed().textContent).toBe('NEW REPORT'); expect(displayed().style.transform).toBe('');
});
