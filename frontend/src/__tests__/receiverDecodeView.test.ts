import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { addDecoderRaster, addDecoderResult, decoderLog, forgetDecoderLog, type DecoderLogEntry } from '../sdr/decoderLog';
import { mountReceiverDecodeView } from '../ui/receiverDecodeView';
import { renderDecoderEntries } from '../ui/decoderResults';
import { setLang } from '../core/i18n';

let host: HTMLElement;
const uid = 'decode-ui';
const message = (id: number, text: string, snr = -12): DecoderLogEntry => ({ id, at: 1000 * id, source: '1:1', provider: 'native', mode: 'ft8', kind: 'table', bytes: 64,
  data: { decodes: [{ msg: text, snr_db: snr, audio_hz: 1100 }] } });
const click = (selector: string): void => host.querySelector<HTMLButtonElement>(selector)!.click();
beforeEach(() => { setLang('en'); host = document.createElement('div'); document.body.append(host); });
afterEach(() => { vi.restoreAllMocks(); forgetDecoderLog(uid); document.body.replaceChildren(); });

describe('receiver Decode interaction and live results', () => {
  it('only offers text tools for text results and resets an old filter when the signal mode changes', () => {
    const view = mountReceiverDecodeView(host);
    view.update([message(1, 'CQ TEST')], uid, 'ft8', 0, '');
    const input = host.querySelector<HTMLInputElement>('[data-decode-search]')!;
    input.value = 'TEST'; input.dispatchEvent(new Event('input', { bubbles: true }));
    for (const mode of ['am', 'wfm', 'nfm', 'drm', 'drmplus', 'sstv', 'radiofax']) {
      view.update([], uid, mode, 0, '');
      expect(host.querySelector<HTMLInputElement>('[data-decode-search]')!.hidden).toBe(true);
      expect(host.querySelector<HTMLButtonElement>('[data-decode-action=clear]')!.hidden).toBe(true);
    }
    view.update([message(2, 'UNRELATED')], uid, 'ft8', 0, '');
    expect(input.value).toBe(''); expect(input.hidden).toBe(false);
    expect(host.querySelector('tbody')?.textContent).toContain('UNRELATED');
    view.dispose();
  });
  it('reuses the AM audio waveform and reads CW speed, tone and tuning from real telemetry', () => {
    const stroke = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ clearRect: vi.fn(), beginPath: vi.fn(),
      moveTo: vi.fn(), lineTo: vi.fn(), stroke } as unknown as CanvasRenderingContext2D);
    const view = mountReceiverDecodeView(host);
    const am = (scope: number[]): DecoderLogEntry => ({ ...message(1, ''), mode: 'am', kind: 'status',
      data: { type: 'am-info', scope, rmsDbFs: -20, peakDbFs: -6, modulationPct: 30 } });
    view.update([am([0, 0.1, -0.1])], uid, 'am', 0, '');
    const canvas = host.querySelector('[data-audio-scope]')!;
    view.update([am([0, 0.2, -0.2])], uid, 'am', 0, '');
    expect(host.querySelector('[data-audio-scope]')).toBe(canvas); expect(stroke).toHaveBeenCalledTimes(4);
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('-20.0 dBFS');
    view.update([{ ...message(2, ''), mode: 'cw', kind: 'status', data: { type: 'cw-level', wpm: 18, pitchHz: 700, afcHz: -12, rms: 0.1, keyed: true } }], uid, 'cw', 0, '');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('18.0 WPM');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('700.0 Hz');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('-12.0 Hz');
    view.dispose();
  });
  it('merges message batches into one sortable table and retains controls and typed filtering across updates', () => {
    const view = mountReceiverDecodeView(host), first = message(1, 'CQ TEST AA00', -20), second = message(2, 'CQ OTHER BB00', -2);
    view.update([second, first], uid, 'ft8', 0, '');
    expect(host.querySelectorAll('.mc-message-table')).toHaveLength(1);
    expect(host.querySelector('tbody tr')?.textContent).toContain('OTHER');
    const sort = host.querySelector<HTMLButtonElement>('[data-sort=snr]')!; sort.focus();
    view.update([message(3, 'CQ NEW CC00', -10), second, first], uid, 'ft8', 0, '');
    expect(document.activeElement).toBe(sort); sort.click();
    expect(host.querySelector('tbody tr')?.textContent).toContain('TEST');
    const input = host.querySelector<HTMLInputElement>('[data-decode-search]')!;
    input.focus(); input.value = 'test'; input.setSelectionRange(1, 3); input.dispatchEvent(new Event('input', { bubbles: true }));
    const row = host.querySelector('tbody tr');
    view.update([message(4, 'UNRELATED'), second, first], uid, 'ft8', 0, '');
    expect(document.activeElement).toBe(input); expect(input.selectionStart).toBe(1); expect(input.selectionEnd).toBe(3);
    expect(host.querySelectorAll('tbody tr')).toHaveLength(1); expect(host.querySelector('tbody tr')).toBe(row);
    click('[data-decode-action=utc]'); expect(host.querySelector('th')?.textContent).toContain('UTC');
    view.dispose();
  });
  it('confirms a local clear while preserving the shared history and accepting subsequent messages', () => {
    addDecoderResult({ provider: 'native', mode: 'ft8', kind: 'table', data: { decodes: [{ msg: 'OLD CQ' }] } }, '1:1', uid);
    const view = mountReceiverDecodeView(host), log = decoderLog(uid);
    view.update(log.entries, uid, 'ft8', 0, '');
    click('[data-decode-action=clear]'); expect(host.querySelector('tbody')?.textContent).toContain('OLD CQ');
    click('[data-decode-action=clear-confirm]'); expect(host.querySelector('tbody tr')).toBeNull();
    expect(decoderLog(uid).entries).toEqual(log.entries);
    addDecoderResult({ provider: 'native', mode: 'ft8', kind: 'table', data: { decodes: [{ msg: 'NEW CQ' }] } }, '1:1', uid);
    view.update(decoderLog(uid).entries, uid, 'ft8', 0, ''); expect(host.querySelector('tbody')?.textContent).toContain('NEW CQ');
    expect(host.querySelector('tbody')?.textContent).not.toContain('OLD CQ');
    view.update([message(1, 'OTHER CHANNEL')], 'another-uid', 'ft8', 0, '');
    expect(host.querySelector('tbody')?.textContent).toContain('OTHER CHANNEL'); view.dispose();
  });
  it('keeps the live raster canvas and its actual pixels through row updates, polling, UTC and language changes', () => {
    const draw = vi.fn(), context = { createImageData: (w: number, h: number) => ({ data: new Uint8ClampedArray(w * h * 4) }), putImageData: draw };
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
    const view = mountReceiverDecodeView(host);
    const image = (row: number, rgb: number[]) => addDecoderRaster(uid, 'sstv', '1:1', { id: 'picture', label: 'Martin M1', w: 2, h: 2,
      row, rows: 1, rgb: new Uint8Array(rgb), final: row === 1, partial: false });
    image(0, [255, 0, 0, 0, 255, 0]); view.update(decoderLog(uid).entries, uid, 'sstv', 0, '');
    const canvas = host.querySelector('canvas')!; expect(canvas.width).toBe(2);
    image(1, [0, 0, 255, 255, 255, 255]); view.update(decoderLog(uid).entries, uid, 'sstv', 0, '');
    expect(host.querySelector('canvas')).toBe(canvas);
    expect([...draw.mock.calls.at(-1)![0].data]).toEqual([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
    click('[data-decode-action=utc]'); setLang('zh'); view.update(decoderLog(uid).entries, uid, 'sstv', 0, '');
    expect(host.querySelector('canvas')).toBe(canvas); expect(canvas.isConnected).toBe(true);
    view.dispose(); setLang('en');
  });
  it('keeps received PNG strips loaded when another strip or completion arrives', () => {
    const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';
    const add = (y: number) => addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'image', data: {
      id: 1, label: 'Meteor', w: 1, h: 3, ch: 1, y, rows: 1, pw: 1, ph: 1, png,
    } }, '1:1', uid);
    add(0); renderDecoderEntries(host, decoderLog(uid).entries, uid, false);
    const original = host.querySelector('img')!, article = host.querySelector('article')!;
    add(1); renderDecoderEntries(host, decoderLog(uid).entries, uid, false);
    expect(host.querySelector('img')).toBe(original); expect(host.querySelector('article')).toBe(article);
    expect(host.querySelectorAll('img')).toHaveLength(2);
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'status', data: { type: 'image_end', id: 1, rows: 2 } }, '1:1', uid);
    renderDecoderEntries(host, decoderLog(uid).entries, uid, true);
    expect(host.querySelector('img')).toBe(original); expect(article.textContent).toContain('Complete');
  });
  it('shows real AM and WFM readings directly without requiring original JSON details', () => {
    const view = mountReceiverDecodeView(host);
    const status = (mode: string, data: unknown): DecoderLogEntry => ({ id: 1, at: 1, source: '1:1', provider: 'native', mode, kind: 'status', bytes: 100, data });
    view.update([status('am', { syncEnabled: true, syncLocked: false, carrierHz: 100000000, modulationPct: 51, inputDbFs: -16 })], uid, 'am', 0, '');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('100.000000 MHz');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('51.0 %');
    view.update([status('wfm', { rdsLocked: true, ps: 'RADIO ONE', rt: 'Programme text', stereo: true, rdsPi: 0xABCD })], uid, 'wfm', 0, '');
    expect(host.querySelector('.mc-decode-log')?.textContent).toContain('RADIO ONE');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('Programme text');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('ABCD'); view.dispose();
  });
  it('retains separate live telemetry when noise reduction reports its own status', () => {
    const view = mountReceiverDecodeView(host);
    const add = (type: string, data: object): void => addDecoderResult({ provider: 'native', mode: 'am', kind: 'status', data: { type, ...data } }, '1:1', uid);
    add('am-info', { syncEnabled: true, syncLocked: true, carrierHz: 100000001, modulationPct: 50, inputDbFs: -10 });
    add('nr-status', { state: 'off', reason: null });
    view.update(decoderLog(uid).entries, uid, 'am', 0, '');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('100.000001 MHz');
    expect(host.querySelectorAll('.decoder-result-status')).toHaveLength(1);
    add('am-info', { syncEnabled: true, syncLocked: true, carrierHz: 100000002, modulationPct: 49, inputDbFs: -11 });
    add('nr-status', { state: 'ready', reason: null });
    view.update(decoderLog(uid).entries, uid, 'am', 0, '');
    expect(host.querySelector('.decoder-fields')?.textContent).toContain('100.000002 MHz');
    expect(decoderLog(uid).entries).toHaveLength(2); view.dispose();
  });
  it('keeps decoder failures visible alongside the friendly mode-specific telemetry', () => {
    const view = mountReceiverDecodeView(host);
    for (const [type, data] of [['ft8-state', { bufferedSeconds: 7.5, count: 2 }], ['error', { message: 'Decoder input interrupted' }]] as const) {
      addDecoderResult({ provider: 'native', mode: 'ft8', kind: 'status', data: { type, ...data } }, '1:1', uid);
    }
    view.update(decoderLog(uid).entries, uid, 'ft8', 0, '');
    expect(host.querySelector('.mc-decode-log')?.textContent).toContain('Decoder input interrupted');
    expect(host.querySelector('.mc-decode-log')?.textContent).toContain('7.5 s'); view.dispose();
  });
  it('updates source-clock details on a retained status node after the stream position advances', () => {
    const entry: DecoderLogEntry = { ...message(1, ''), kind: 'status', data: { state: 'receiving' }, timing: {
      input_index: '9007199254740999', input_rate: 62500000, fraction_num: '1', fraction_den: '3', delay_seconds: 0.01, delay_known: true, causal_presentation: false } };
    renderDecoderEntries(host, [entry], uid, false); const node = host.querySelector('article')!;
    renderDecoderEntries(host, [{ ...entry, source: '9:9', timing: { ...entry.timing!, input_index: '9007199254741999' } }], uid, false);
    expect(host.querySelector('article')).toBe(node); expect(node.title).toContain('9:9');
    expect(node.title).toContain('9007199254741999 + 1/3'); expect(node.title).not.toContain('9007199254740999');
  });
});
