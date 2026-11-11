import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { addDecoderRaster, addDecoderResult, decoderLog, forgetDecoderLog, type DecoderLogEntry } from '../sdr/decoderLog';
import { mountReceiverDecodeView } from '../ui/receiverDecodeView';
import { renderDecoderEntries } from '../ui/decoderResults';
import { setLang } from '../core/i18n';
import * as geography from '../ui/ft8GeoText';
import { consumeReceiverResult } from '../ui/nativeWorkbenchStreams';
import { resultExport } from '../ui/receiverExport';
import { ft8SnrPresentation } from '../sdr/ft8Snr';

let host: HTMLElement;
const uid = 'decode-ui';
const message = (id: number, text: string, snr = -12): DecoderLogEntry => ({ id, at: 1000 * id, source: '1:1', provider: 'native', mode: 'ft8', kind: 'table', bytes: 64,
  data: { decodes: [{ msg: text, snr_db: snr, audio_hz: 1100 }] } });
const click = (selector: string): void => host.querySelector<HTMLButtonElement>(selector)!.click();
beforeEach(() => { setLang('en'); host = document.createElement('div'); document.body.append(host); });
afterEach(() => { vi.restoreAllMocks(); forgetDecoderLog(uid); document.body.replaceChildren(); });

describe('receiver Decode interaction and live results', () => {
  it('hides RF SNR by default and ignores its updates until measurement is enabled', () => {
    const view = mountReceiverDecodeView(host);
    const measurement = (value: number): DecoderLogEntry => ({ ...message(10, ''), kind: 'status', mode: 'wfm',
      data: { type: 'rf-measurement', measurement: { value, state: 'conditional', reference_bandwidth_hz: 180000 } } });
    try {
      view.update([measurement(12)], uid, 'wfm', 0, '');
      expect(host.textContent).not.toContain('RF SNR');
      const observer = new MutationObserver(() => {});
      observer.observe(host, { subtree: true, childList: true, attributes: true, characterData: true });
      view.update([measurement(13)], uid, 'wfm', 0, '');
      expect(observer.takeRecords()).toHaveLength(0);
      observer.disconnect();
      view.update([measurement(14)], uid, 'wfm', 0, '', undefined, true);
      expect(host.textContent).toContain('RF SNR');
      expect(host.textContent).toContain('14.0 dB');
      view.update([measurement(14)], uid, 'wfm', 0, '');
      expect(host.textContent).not.toContain('RF SNR');
    } finally { view.dispose(); }
  });
  it('keeps alternating RF and demodulator status cards in place', () => {
    const view = mountReceiverDecodeView(host);
    const add = (type: string, value: number): void => {
      addDecoderResult({ provider: 'native', mode: 'wfm', kind: 'status', data: type === 'rf-measurement'
        ? { type, measurement: { value, state: 'conditional', reference_bandwidth_hz: 180000 } }
        : { type, rdsLocked: true, ps: 'RADIO', stereo: true } }, '1:1', uid);
      view.update(decoderLog(uid).entries, uid, 'wfm', 0, '', undefined, true);
    };
    try {
      add('rf-measurement', 12); add('wfm-info', 0);
      const cards = [...host.querySelectorAll('.mc-decode-log > .decoder-result')];
      expect(cards).toHaveLength(2);
      const observer = new MutationObserver(() => {});
      observer.observe(host.querySelector('.mc-decode-log')!, { childList: true });
      for (let i = 0; i < 5; i++) { add('rf-measurement', 12 + i); add('wfm-info', 0); }
      expect([...host.querySelectorAll('.mc-decode-log > .decoder-result')]).toEqual(cards);
      expect(observer.takeRecords()).toHaveLength(0);
      observer.disconnect();
    } finally { view.dispose(); }
  });
  it('shows near-zero estimates without negative zero and retains the original value in hover and export', () => {
    const view = mountReceiverDecodeView(host);
    try {
      const data = { ...message(1, 'CQ TEST AA00'), data: { decodes: [{ msg: 'CQ TEST AA00', snr_db: -0.2,
        measurement: { value: -0.2, quality: 'good', value_kind: 'estimate' } }] } };
      view.update([data], uid, 'ft8', 0, '');
      expect(host.querySelector('tbody tr')?.children[1].textContent).toBe('0');
      expect((host.querySelector('tbody tr')?.children[1] as HTMLElement).title).toContain('Estimate -0.20 dB');
      expect(resultExport([data], uid, 'csv')).toContain('"-0.2"');
      for (const [value, text] of [[-0, '0'], [-0.001, '0'], [0.2, '0'], [-0.51, '-1'], [0.51, '1']] as const)
        expect(ft8SnrPresentation(value).text).toBe(text);
    } finally { view.dispose(); }
  });
  it('retains SNR value and quality through native ingress, themes, languages and exports', () => {
    const view = mountReceiverDecodeView(host);
    const definition = { metric_type: 'ft8_snr', unit: 'dB', state: 'conditional', location: 'original_iq_before_decoder_cancellation',
      method: 'decoded_hann_matched_filters_with_exponential_tail_corrected_noise', averaging_seconds: 9.48, reference_bandwidth_hz: 2500 };
    const measurement = { ...definition, value: 16.25, quality: 'degraded', value_kind: 'estimate', quality_reasons: ['off_tone_interference'] };
    const envelope = { version: 1 as const, channel: 1, channel_id: 8, epoch: 1, seq: 1, input_index: '100', input_fraction_num: '0', input_fraction_den: '1',
      input_rate: 12000, delay_seconds: 0, delay_known: true, causal_presentation: false, mode: 'ft8' };
    try {
      consumeReceiverResult(uid, { ...envelope, event: { type: 'ft8', text: 'CQ TEST AA00', snrDb: 16.25, measurement } });
      const log = decoderLog(uid);
      for (const theme of ['dark', 'light']) for (const lang of ['en', 'zh'] as const) {
        document.documentElement.dataset.theme = theme; setLang(lang); view.update(log.entries, uid, 'ft8', 0, '');
        const cell = host.querySelector<HTMLTableCellElement>('.ft8-snr-degraded')!;
        expect(cell.textContent).toBe('16');
        expect(cell.title).toContain(lang === 'en' ? 'Noise references contain interference' : '噪声参考受到干扰');
        expect(cell.title).toContain('2500 Hz');
      }
      expect(JSON.parse(resultExport(log.entries, uid, 'jsonl')).data.decodes[0]).toMatchObject({ snr_db: 16.25, measurement });
      expect(resultExport(log.entries, uid, 'csv')).toContain('"estimate","degraded","","off_tone_interference","2500"');
      consumeReceiverResult(uid, { ...envelope, seq: 2, event: { type: 'ft8', text: 'CQ OTHER AA00', snrDb: null,
        measurement: { ...definition, state: 'unavailable', value: null, quality: 'unavailable', value_kind: 'unavailable', reason: 'signal_not_resolved' } } });
      view.update(decoderLog(uid).entries, uid, 'ft8', 0, '');
      const unavailable = [...host.querySelectorAll('td')].find(cell => cell.textContent === '—' && cell.title.includes('无法从背景'));
      expect(unavailable).toBeDefined();
    } finally { view.dispose(); setLang('en'); delete document.documentElement.dataset.theme; }
  });
  it('does not rebuild retained FT8 rows or measure an empty transcript for live status updates', () => {
    const view = mountReceiverDecodeView(host), geo = vi.spyOn(geography, 'ft8GeoTexts');
    const entries = [message(1, 'CQ W1ABC FN42'), message(2, 'CQ JA1ABC PM95')];
    view.update(entries, uid, 'ft8', 0, '');
    const row = host.querySelector('tr[data-key="1:0"]');
    expect(geo).toHaveBeenCalledTimes(2);
    geo.mockClear();
    const scroll = vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get');
    try {
      for (let i = 0; i < 5; i++) view.update([{ ...message(10 + i, ''), kind: 'status', data: { type: 'ft8-state', attempts: i } }, ...entries], uid, 'ft8', 0, '');
      expect(host.querySelector('tr[data-key="1:0"]')).toBe(row);
      expect(geo).not.toHaveBeenCalled(); expect(scroll).not.toHaveBeenCalled();
      setLang('zh'); view.update(entries, uid, 'ft8', 0, '');
      expect(geo).toHaveBeenCalledTimes(2);
      expect(host.querySelector('.mc-message-country')!.textContent).toMatch(/美国|日本/);
    } finally { view.dispose(); }
  });
  it('renders a retained long CW transmission without exceeding function argument limits', () => {
    const view = mountReceiverDecodeView(host);
    try {
      for (let i = 0; i < 5; i++) addDecoderResult({ provider: 'native', mode: 'cw', kind: 'text',
        data: { text: 'E'.repeat(30_000), endsLine: false, share: 1 } }, '1:1', uid);
      addDecoderResult({ provider: 'native', mode: 'cw', kind: 'text', data: { text: 'TAIL', endsLine: true, share: 0.2 } }, '1:1', uid);
      const log = decoderLog(uid);
      expect(log.entries).toHaveLength(1);
      expect(log.entries[0].bytes).toBeLessThan(24 * 1024 * 1024);
      expect(() => view.update(log.entries, uid, 'cw', log.evicted, '', '1:1')).not.toThrow();
      expect(host.querySelector('.cw-text')?.textContent).toBe('E'.repeat(150_000) + 'TAIL');
      expect(host.querySelector('.cw-ch.weak')?.textContent).toBe('TAIL');
    } finally { view.dispose(); }
  });

  it('does not reset result filters or export menus while new results arrive', () => {
    const view = mountReceiverDecodeView(host);
    view.update([message(1, 'CQ TEST')], uid, 'ft8', 0, '');
    for (const [selector, value] of [['[data-result-mode]', 'all'], ['[data-export-format]', 'jsonl']]) {
      const select = host.querySelector<HTMLSelectElement>(selector)!;
      select.value = value; select.dispatchEvent(new Event('change', { bubbles: true }));
      const setter = vi.spyOn(select, 'value', 'set');
      for (const focused of [false, true]) {
        if (focused) select.focus();
        for (let i = 0; i < 5; i++) view.update([message(1, 'CQ TEST'), message(i + 2, 'NEW')], uid, 'ft8', 0, '');
        expect(host.querySelector(selector)).toBe(select);
        expect(select.value).toBe(value); expect(setter).not.toHaveBeenCalled();
      }
      select.blur();
    }
    view.dispose();
  });
  it('presents RTTY as continuous text plus metrics without repeating snapshot text', () => {
    const view = mountReceiverDecodeView(host);
    const entries: DecoderLogEntry[] = [
      { ...message(1, ''), provider: 'onair-rtty', mode: 'rtty', kind: 'text', data: { text: 'CQ CQ DE TEST', endsLine: false } },
      { ...message(2, ''), provider: 'onair-rtty', mode: 'rtty', kind: 'status', data: { type: 'rtty-state', state: 'receiving', text: 'DO NOT REPEAT', baud: 45.45, shiftHz: 170, framingErrors: 2, characters: 13 } },
    ];
    view.update(entries, uid, 'rtty', 0, '');
    expect(host.querySelector('.mc-text-transcript')?.textContent).toContain('CQ CQ DE TEST');
    expect(host.querySelector('.decoder-metrics')?.textContent).toContain('45.45');
    expect(host.textContent).not.toContain('DO NOT REPEAT');
    expect(host.querySelectorAll('.decoder-result-text')).toHaveLength(0);
    view.dispose();
  });
  it('renders CW as chronological transmissions and clears only this view even when a live line grows', () => {
    const view = mountReceiverDecodeView(host), other = document.createElement('div'), sibling = mountReceiverDecodeView(other);
    const push = (text: string, endsLine = false, share = 1, source = '1:1') => {
      addDecoderResult({ provider: 'native', mode: 'cw', kind: 'text', data: { text: text + (endsLine ? '\n' : ''), endsLine, share } }, source, uid);
      view.update(decoderLog(uid).entries, uid, 'cw', 0, '', source);
      sibling.update(decoderLog(uid).entries, uid, 'cw', 0, '', source);
    };
    push('C'); push('Q'); push(' DE '); push('TEST', true);
    push('N', false, 0.2); push('EW');
    expect(host.querySelectorAll('.cw-line')).toHaveLength(2);
    expect(host.querySelector('.cw-text')?.textContent).toBe('CQ DE TEST');
    expect(host.querySelector('.cw-ch.weak')?.textContent).toBe('N');
    expect(host.querySelectorAll('.decoder-result-text')).toHaveLength(0);
    const current = host.querySelector('.cw-line.current');
    push(' CQ'); expect(host.querySelector('.cw-line.current')).toBe(current);
    const search = host.querySelector<HTMLInputElement>('[data-decode-search]')!;
    search.value = 'DE TEST'; search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(host.querySelectorAll('.cw-line')).toHaveLength(1);
    search.value = ''; search.dispatchEvent(new Event('input', { bubbles: true }));
    click('[data-decode-action=clear]'); click('[data-decode-action=clear-confirm]');
    expect(host.querySelectorAll('.cw-line')).toHaveLength(0);
    push(' APPENDED');
    expect(host.querySelector('.cw-text')?.textContent).toBe(' APPENDED▌');
    expect(other.querySelector('.cw-line.current')?.textContent).toContain('NEW CQ APPENDED');
    push('DIFFERENT', false, 1, '2:2');
    expect(host.querySelectorAll('.cw-line')).toHaveLength(2);
    view.update(decoderLog(uid).entries, uid, 'cw', 0, 'Receiver stopped', null);
    expect(host.querySelector('.cw-cursor')).toBeNull();
    view.dispose(); sibling.dispose();
  });
  it('shows localized FT8 countries and transmitted grid coordinates, with sortable columns', () => {
    const view = mountReceiverDecodeView(host);
    const entries = [message(1, 'CQ W1ABC FN42'), message(2, 'W1ABC JA1ABC PM95'), message(3, 'JA1ABC W1ABC R-12')];
    view.update(entries, uid, 'ft8', 0, '');
    const cell = (id: number, suffix: string) => host.querySelector(`tr[data-key="${id}:0"] .mc-message-${suffix}`)!.textContent;
    expect(cell(1, 'country')).toBe('United States');
    expect(cell(1, 'location')).toBe('42.5°N 71.0°W');
    expect(cell(2, 'country')).toBe('Japan → United States');
    expect(cell(2, 'location')).toBe('35.5°N 139.0°E');
    expect(cell(3, 'location')).toBe('—');
    click('[data-sort=country]');
    expect(host.querySelector('tbody tr')?.getAttribute('data-key')).toBe('2:0');
    setLang('zh'); view.update(entries, uid, 'ft8', 0, '');
    expect(cell(1, 'country')).toBe('美国');
    expect(host.querySelector('thead')!.textContent).toContain('国家/地区');
    view.dispose();
  });
  it('highlights FT8 QSO peers locally and keeps hover through new messages, clearing on identity changes', () => {
    const other = document.createElement('div'); document.body.append(other);
    const view = mountReceiverDecodeView(host), sibling = mountReceiverDecodeView(other);
    const entries = [message(1, 'CQ W1ABC FN42'), message(2, 'W1ABC K2XYZ -12'),
      message(3, 'K2XYZ W1ABC R-08'), message(4, 'W1ABC N3DEF 73'), message(5, 'CQ G4ZZZ IO91')];
    view.update(entries, uid, 'ft8', 0, ''); sibling.update(entries, uid, 'ft8', 0, '');
    const row = (id: number) => host.querySelector<HTMLElement>(`tr[data-key="${id}:0"]`)!;
    row(2).firstElementChild!.dispatchEvent(new MouseEvent('pointerover', { bubbles: true }));
    expect(row(2).classList.contains('qso-focus')).toBe(true);
    expect(row(3).classList.contains('qso-peer')).toBe(true);
    expect(host.querySelectorAll('.qso-peer')).toHaveLength(1);
    expect(other.querySelectorAll('.qso-peer,.qso-focus')).toHaveLength(0);
    // Moving between cells of the same row must not clear the session.
    row(2).firstElementChild!.dispatchEvent(new MouseEvent('pointerout', { bubbles: true, relatedTarget: row(2).lastElementChild }));
    expect(row(3).classList.contains('qso-peer')).toBe(true);
    view.update([...entries, message(6, 'W1ABC K2XYZ 73')], uid, 'ft8', 0, '');
    expect(row(6).classList.contains('qso-peer')).toBe(true);
    row(1).dispatchEvent(new MouseEvent('pointerover', { bubbles: true }));
    expect(host.querySelectorAll('.qso-peer')).toHaveLength(4);
    expect(row(5).classList.contains('qso-peer')).toBe(false);
    row(1).dispatchEvent(new MouseEvent('pointerout', { bubbles: true }));
    expect(host.querySelectorAll('.qso-peer,.qso-focus')).toHaveLength(0);
    row(2).focus(); expect(row(3).classList.contains('qso-peer')).toBe(true);
    view.update(entries, 'different-channel', 'ft8', 0, '');
    expect(host.querySelectorAll('.qso-peer,.qso-focus')).toHaveLength(0);
    row(2).dispatchEvent(new MouseEvent('pointerover', { bubbles: true }));
    const search = host.querySelector<HTMLInputElement>('[data-decode-search]')!;
    search.value = 'G4ZZZ'; search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(host.querySelectorAll('.qso-peer,.qso-focus')).toHaveLength(0);
    view.dispose(); sibling.dispose();
  });
  it('presents DRM quality, frame counts, services and constellation and clears the plot after lock loss', () => {
    const view = mountReceiverDecodeView(host);
    const data = { type: 'drm', snrDb: 20, lines: ['FAC MER 23.5 dB', '42 MSC frames, 84 audio AUs, FAC ok 40 err 2'],
      points: [{ re: 0.7, im: -0.7 }], status: { locked: true, mode: 'B', stationLabel: 'Station <one>',
        facOk: 40, facErrors: 2, sdcOk: 9, carrierOffsetHz: 1.2,
        services: [{ label: 'News', serviceId: 123, audioCoding: 3, sampleRate: 24000, channels: 2 }] } };
    const entry: DecoderLogEntry = { ...message(1, ''), kind: 'status', mode: 'drm', data };
    view.update([entry], uid, 'drm', 0, '');
    expect(host.querySelector('.decoder-metrics')?.textContent).toContain('23.5 dB');
    expect(host.querySelector('.decoder-metrics')?.textContent).toContain('MSC frames42');
    expect(host.querySelector('.decoder-service')?.textContent).toContain('NewsID 123 · xHE-AAC · 24 kHz · 2 ch');
    expect(host.querySelector('.decoder-signal-heading')?.textContent).toBe('Station <one>');
    expect(host.querySelectorAll('.decoder-constellation circle:not([class])')).toHaveLength(1);
    view.update([{ ...entry, data: { ...data, status: { ...data.status, locked: false } } }], uid, 'drm', 0, '');
    expect(host.querySelectorAll('.decoder-constellation circle:not([class])')).toHaveLength(0);
    view.dispose();
  });
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
    const painted = new Uint8ClampedArray(16);
    const draw = vi.fn((image: { data: Uint8ClampedArray }, _x: number, y: number) => painted.set(image.data, y * 8));
    const context = { createImageData: (w: number, h: number) => ({ data: new Uint8ClampedArray(w * h * 4) }), putImageData: draw };
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
    const view = mountReceiverDecodeView(host);
    const image = (row: number, rgb: number[]) => addDecoderRaster(uid, 'sstv', '1:1', { id: 'picture', label: 'Martin M1', w: 2, h: 2,
      row, rows: 1, rgb: new Uint8Array(rgb), final: row === 1, partial: false });
    image(0, [255, 0, 0, 0, 255, 0]); view.update(decoderLog(uid).entries, uid, 'sstv', 0, '');
    const canvas = host.querySelector('canvas')!; expect(canvas.width).toBe(2);
    image(1, [0, 0, 255, 255, 255, 255]); view.update(decoderLog(uid).entries, uid, 'sstv', 0, '');
    expect(host.querySelector('canvas')).toBe(canvas);
    expect([...painted]).toEqual([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
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
