import { describe, expect, it, vi } from 'vitest';
import { DecodeSequence, MAX_DECODE_BYTES, type DecodeEnvelope } from '../sdr/decodeProtocol';

function frame(overrides: Partial<DecodeEnvelope> = {}): ArrayBuffer {
  const value: DecodeEnvelope = { version: 1, channel: 1, channel_id: 17, epoch: 9, seq: 0,
    input_index: '9007199254740993', input_rate: 62500000, input_fraction_num: '0', input_fraction_den: '1',
    delay_seconds: 0.001, delay_known: true, causal_presentation: false, mode: 'cw', event: { type: 'start' }, ...overrides };
  const text = new TextEncoder().encode(JSON.stringify(value));
  const data = new ArrayBuffer(text.length + 8);
  new Uint8Array(data).set([83, 68, 69, 50]);
  new DataView(data).setUint32(4, text.length, true);
  new Uint8Array(data, 8).set(text);
  return data;
}
function amEvent(overrides: Record<string, unknown> = {}): DecodeEnvelope['event'] {
  return { type: 'am-info', syncEnabled: true, syncLocked: true, amSyncGeneration: 0, carrierHz: 9_810_001.25, carrierOffsetHz: 1.25,
    modulationPct: 42, inputDbFs: -26, rmsDbFs: -32, peakDbFs: -24, scope: [-1, 0, 1],
    ...overrides } as DecodeEnvelope['event'];
}
describe('native decoded stream boundaries', () => {
  it('keeps all three channel clocks independent when one changes identity', () => {
    const stream = new DecodeSequence();
    for (const channel of [1, 2, 3]) stream.accept(frame({ channel, channel_id: channel }));
    stream.accept(frame({ channel: 2, channel_id: 4, event: { type: 'start' } }));
    const event = { type: 'cw', text: 'CQ', keyed: true, rms: 0.3 };
    expect(stream.accept(frame({ channel: 1, channel_id: 1, seq: 1, event })).event.text).toBe('CQ');
    expect(stream.accept(frame({ channel: 3, channel_id: 3, seq: 1, event })).channel).toBe(3);
    expect(stream.accept(frame({ channel: 2, channel_id: 4, seq: 1, event })).channel_id).toBe(4);
  });
  it('rejects loss and old-identity output instead of mixing receiver state', () => {
    const stream = new DecodeSequence();
    stream.accept(frame());
    expect(() => stream.accept(frame({ seq: 2, event: { type: 'cw' } }))).toThrow(/sequence gap/);
    stream.accept(frame({ channel_id: 18 }));
    expect(() => stream.accept(frame())).toThrow(/Stale/);
    expect(() => stream.accept(frame({ seq: 1, event: { type: 'cw' } }))).toThrow(/identity/);
  });
  it.each(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm', 'ft8', 'sstv', 'drm', 'drmplus'])('requires a fresh identity for a switch to %s', (mode) => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: mode === 'cw' ? 'am' : 'cw' }));
    expect(() => stream.accept(frame({ mode, seq: 1, event: { type: 'nr-status' } }))).toThrow(/mode changed/);
    stream.accept(frame({ channel_id: 18, mode }));
    expect(stream.accept(frame({ channel_id: 18, mode, seq: 1, event: { type: 'nr-status' } })).mode).toBe(mode);
    expect(() => stream.accept(frame({ channel_id: 18, mode, seq: 2 }))).toThrow(/mode changed/);
  });
  it('compares rational source clocks above the JavaScript exact integer range', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ input_fraction_num: '2', input_fraction_den: '3' }));
    stream.accept(frame({ seq: 1, input_fraction_num: '4', input_fraction_den: '6', event: { type: 'cw' } }));
    expect(() => stream.accept(frame({ seq: 2, input_fraction_num: '1', input_fraction_den: '2', event: { type: 'cw' } }))).toThrow(/backwards/);
    const result = stream.accept(frame({ seq: 2, input_index: '9007199254740994', event: { type: 'cw' } }));
    expect(result.input_index).toBe('9007199254740994');
  });
  it('retains delayed FT8 window anchors while preserving stream sequence', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'ft8' }));
    stream.accept(frame({ mode: 'ft8', seq: 1, input_index: '9007199254741993', event: { type: 'ft8-state' } }));
    const result = stream.accept(frame({ mode: 'ft8', seq: 2, input_index: '9007199254740993', event: {
      type: 'ft8', text: 'CQ W1AW FN31', windowStart: '9007199254740993' } }));
    expect(result.input_index).toBe('9007199254740993');
    expect(() => stream.accept(frame({ mode: 'ft8', seq: 3, input_index: '9007199254740994', event: { type: 'ft8-state' } }))).toThrow(/backwards/);
    expect(stream.accept(frame({ mode: 'ft8', seq: 3, input_index: '9007199254741994', event: { type: 'ft8-state' } })).seq).toBe(3);
  });
  it('keeps causal codec provenance even when contributor anchors arrive out of order', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'drm', causal_presentation: true }));
    stream.accept(frame({ mode: 'drm', seq: 1, causal_presentation: true, input_index: '9007199254741993', event: { type: 'drm' } }));
    const result = stream.accept(frame({ mode: 'drm', seq: 2, causal_presentation: true,
      input_index: '9007199254740993', event: { type: 'drm' } }));
    expect(result.input_index).toBe('9007199254740993');
    expect(result.causal_presentation).toBe(true);
  });
  it('accepts successful NR status with an explicit absent failure reason', () => {
    const stream = new DecodeSequence();
    stream.accept(frame());
    const result = stream.accept(frame({ seq: 1, event: { type: 'nr-status', state: 'ready', reason: null } }));
    expect(result.event.reason).toBeNull();
    expect(result.event.state).toBe('ready');
  });
  it('rejects unavailable clocks, invalid fractions and malformed lengths', () => {
    const stream = new DecodeSequence();
    expect(() => stream.accept(frame({ input_index: '18446744073709551616' }))).toThrow(/overflow/);
    expect(() => stream.accept(frame({ input_fraction_den: '0' }))).toThrow(/fraction/);
    expect(() => stream.accept(frame({ input_fraction_num: '2', input_fraction_den: '2' }))).toThrow(/fraction/);
    const invalid = frame();
    new DataView(invalid).setUint32(4, invalid.byteLength, true);
    expect(() => stream.accept(invalid)).toThrow(/header/);
    expect(() => stream.accept(new ArrayBuffer(MAX_DECODE_BYTES + 9))).toThrow(/size/);
  });
  it('accepts the full JSON payload limit independently of its eight-byte header', () => {
    const empty = frame({ event: { type: 'start', text: '' } });
    const text = 'x'.repeat(MAX_DECODE_BYTES - (empty.byteLength - 8));
    expect(new DecodeSequence().accept(frame({ event: { type: 'start', text } })).event.text).toBe(text);
    expect(() => new DecodeSequence().accept(frame({ event: { type: 'start', text: text + 'x' } }))).toThrow(/size/);
  });
  it('requires an explicit start and rejects mode payloads with nonnumeric telemetry', () => {
    const stream = new DecodeSequence();
    expect(() => stream.accept(frame({ seq: 1, event: { type: 'cw' } }))).toThrow(/gap/);
    expect(() => stream.accept(frame({ event: { type: 'cw' } }))).toThrow(/start/);
    const bad = JSON.parse(new TextDecoder().decode(new Uint8Array(frame(), 8)));
    bad.event = { type: 'cw', rms: 'invalid' };
    const text = new TextEncoder().encode(JSON.stringify(bad));
    const buffer = new ArrayBuffer(8 + text.length);
    new Uint8Array(buffer).set([83, 68, 69, 50]);
    new DataView(buffer).setUint32(4, text.length, true);
    new Uint8Array(buffer, 8).set(text);
    expect(() => stream.accept(buffer)).toThrow(/rms/);
  });
  it('preserves nullable AM measurements and accepts the bounded scope endpoints', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    const unknown = amEvent({ carrierHz: null, carrierOffsetHz: null, modulationPct: null, inputDbFs: null, scope: [] });
    expect(stream.accept(frame({ mode: 'am', seq: 1, event: unknown })).event).toEqual(unknown);
    const bounded = amEvent({ syncEnabled: false, syncLocked: false, carrierOffsetHz: 0,
      modulationPct: 0, inputDbFs: 0, rmsDbFs: 0, peakDbFs: 0, scope: Array.from({ length: 256 }, (_, i) => i - 128) });
    expect(stream.accept(frame({ mode: 'am', seq: 2, event: bounded })).event).toEqual(bounded);
  });
  it('requires an unsigned 32-bit AM sync generation without advancing rejected events', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const amSyncGeneration of [undefined, null, '0', false, -1, 0.5, 0x100000000]) {
      expect(() => stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ amSyncGeneration }) }))).toThrow(/AM sync generation/);
    }
    expect(stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ amSyncGeneration: 0 }) })).event.amSyncGeneration).toBe(0);
    expect(stream.accept(frame({ mode: 'am', seq: 2, event: amEvent({ amSyncGeneration: 0xffffffff }) })).event.amSyncGeneration).toBe(0xffffffff);
  });
  it.each(['syncEnabled', 'syncLocked'])('requires an AM %s boolean', (key) => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const invalid of [undefined, null, 0, 'true']) {
      expect(() => stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ [key]: invalid }) }))).toThrow(/AM sync/);
    }
  });
  it.each(['carrierHz', 'carrierOffsetHz', 'modulationPct', 'inputDbFs'])('requires an explicit nullable AM %s measurement', (key) => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const invalid of [undefined, '0', false, {}]) {
      expect(() => stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ [key]: invalid }) }))).toThrow(key);
    }
  });
  it.each(['rmsDbFs', 'peakDbFs'])('requires a numeric AM %s measurement', (key) => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const invalid of [undefined, null, '0', false]) {
      expect(() => stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ [key]: invalid }) }))).toThrow(key);
    }
  });
  it('rejects missing, oversized, and malformed AM scopes without advancing the stream', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const scope of [undefined, null, {}, Array(257).fill(0), [null], ['0'], [true]]) {
      expect(() => stream.accept(frame({ mode: 'am', seq: 1, event: amEvent({ scope }) }))).toThrow(/AM scope/);
    }
    expect(stream.accept(frame({ mode: 'am', seq: 1, event: amEvent() })).seq).toBe(1);
  });
  it('keeps WFM peak text while rejecting AM peak text and numeric WFM peaks', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'wfm' }));
    expect(stream.accept(frame({ mode: 'wfm', seq: 1, event: { type: 'wfm-info', peakDbFs: '-7.0' } })).event.peakDbFs).toBe('-7.0');
    expect(() => stream.accept(frame({ mode: 'wfm', seq: 2, event: { type: 'wfm-info', peakDbFs: -7 } }))).toThrow(/peakDbFs/);
    expect(() => stream.accept(frame({ mode: 'am', seq: 2, event: amEvent({ peakDbFs: '-7.0' }) }))).toThrow(/peakDbFs/);
  });
  it('rejects non-finite AM wire numbers, including otherwise nullable measurements', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'am' }));
    for (const key of ['carrierHz', 'carrierOffsetHz', 'modulationPct', 'inputDbFs', 'rmsDbFs', 'peakDbFs', 'scope']) {
      const event = amEvent({ [key]: key === 'scope' ? [123456789] : 123456789 });
      const text = new TextEncoder().encode(new TextDecoder().decode(new Uint8Array(frame({ mode: 'am', seq: 1, event }), 8))
        .replace('123456789', '1e999'));
      const data = new ArrayBuffer(8 + text.length);
      new Uint8Array(data).set([83, 68, 69, 50]);
      new DataView(data).setUint32(4, text.length, true);
      new Uint8Array(data, 8).set(text);
      expect(() => stream.accept(data)).toThrow(key);
    }
  });
  it('requires a fresh identity for a switch to radiofax and accepts its state telemetry', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'cw' }));
    expect(() => stream.accept(frame({ mode: 'radiofax', seq: 1, event: { type: 'radiofax-state', running: true,
      stage: 'searching', lines: 0, clockStatus: 'nominal', clockSource: 'nominal', modulation: 'fm', polarity: 'normal',
      clockConfidence: 0, reason: null } }))).toThrow(/mode changed/);
    stream.accept(frame({ channel_id: 18, mode: 'radiofax' }));
    const state: DecodeEnvelope['event'] = { type: 'radiofax-state', running: true, stage: 'receiving', hasPicture: true,
      signalLevel: 0.4, carrierCoherence: 0.8, ioc: 576, lpm: 120, modulation: 'fm', polarity: 'normal', width: 1810,
      lines: 43, elapsedSeconds: 21.5, clockStatus: 'locked', clockSource: 'phasing', phasePixels: -1.25, clockPpm: 0.5,
      clockConfidence: 0.9, complete: false, partial: true, reason: null };
    expect(stream.accept(frame({ channel_id: 18, mode: 'radiofax', seq: 1, event: state })).event).toEqual(state);
    for (const bad of [{ stage: 'unknown' }, { ioc: 352 }, { modulation: 'usb' }, { polarity: 'flipped' },
      { clockStatus: 'fast' }, { clockSource: 'guess' }, { signalLevel: 'high' }, { clockConfidence: null },
      { lines: [] }, { lines: -1 }, { lines: 2401 }, { lines: 1.5 }]) {
      expect(() => stream.accept(frame({ channel_id: 18, mode: 'radiofax', seq: 2,
        event: { ...state, ...bad } as unknown as DecodeEnvelope['event'] }))).toThrow();
    }
  });
  it('requires explicit radiofax missing rows and nullable reason on the wire', () => {
    const meta = { serial: '1', epoch: 'meta', hz: null, ioc: 576, lpm: 120, lines: 3,
      complete: false, partial: false, segmentIndex: 0, transmissionId: '1', missingRows: [1], reason: null };
    const event = { type: 'radiofax-image', width: 1, row: 0, rows: 3, revision: 0, gray: 'AYAC', meta };
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'radiofax' }));
    expect(stream.accept(frame({ mode: 'radiofax', seq: 1, event })).event.meta!.missingRows).toEqual([1]);
    for (const extra of [{ missingRows: undefined }, { reason: undefined }, { missingRows: [1, 1] },
      { missingRows: [2, 1] }, { missingRows: [-1] }, { missingRows: [3] }, { missingRows: [0.5] }, { reason: false }]) {
      expect(() => stream.accept(frame({ mode: 'radiofax', seq: 2,
        event: { ...event, meta: { ...meta, ...extra } } as DecodeEnvelope['event'] }))).toThrow(/metadata/);
    }
    for (const transmissionId of ['fax', '', '01', '-1', '1.0', '18446744073709551616', 1, Number.MAX_SAFE_INTEGER + 1, null, undefined]) {
      expect(() => stream.accept(frame({ mode: 'radiofax', seq: 2,
        event: { ...event, meta: { ...meta, transmissionId } } as DecodeEnvelope['event'] }))).toThrow(/transmission id/);
    }
    expect(stream.accept(frame({ mode: 'radiofax', seq: 2,
      event: { ...event, meta: { ...meta, transmissionId: '18446744073709551615' } } })).event.meta!.transmissionId).toBe('18446744073709551615');
  });
  it('accepts radiofax rows and NR status together while keeping DRM text rows strict', () => {
    const stream = new DecodeSequence();
    stream.accept(frame({ mode: 'radiofax' }));
    expect(stream.accept(frame({ mode: 'radiofax', seq: 1,
      event: { type: 'nr-status', state: 'off', reason: null } })).event.state).toBe('off');
    stream.accept(frame({ channel_id: 18, mode: 'drm' }));
    expect(stream.accept(frame({ channel_id: 18, mode: 'drm', seq: 1,
      event: { type: 'drm', lines: ['CQ'] } })).event.lines).toEqual(['CQ']);
    expect(() => stream.accept(frame({ channel_id: 18, mode: 'drm', seq: 2,
      event: { type: 'drm', lines: 43 } as unknown as DecodeEnvelope['event'] }))).toThrow(/lines/);
  });
  it('clears stale AM locks across warnings, fatal errors, stream boundaries, reset, and disconnect', async () => {
    class Receiver {
      static current: Receiver;
      onmessage: ((event: { data: { type: string; envelopes?: DecodeEnvelope[]; message?: string } }) => void) | null = null;
      onerror: ((event: { message: string }) => void) | null = null;
      constructor() { Receiver.current = this; }
      postMessage(): void {}
      terminate(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('Worker', Receiver);
    try {
      // Load a fresh stream and UI together after installing Worker so lifecycle state is isolated.
      const ui = await import('../ui/decodeWindow');
      const { sdrAmSync } = await import('../ui/sdrState');
      const { setLang } = await import('../core/i18n');
      const stream = await import('../sdr/iqStream');
      document.body.innerHTML = `<div id="decode-window"><span id="am-title-badge"></span>
        <div class="decode-am-carrier"><span id="am-carrier"></span><label>Synchronous <button id="am-sync-toggle"></button></label></div>
        <span id="am-carrier-offset"></span><span id="am-modulation"></span><span id="am-input"></span>
        <span id="am-rms"></span><span id="am-peak"></span><div id="am-empty"></div></div>`;
      setLang('en');
      sdrAmSync.reset();
      sdrAmSync.confirm(true);
      ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {}, onAmSync: () => {} });
      ui.setDecodeActive(true);
      ui.setDecodeChannelMode(1, 'am');
      ui.confirmAmSyncGeneration(0);
      if (!ui.decodeWindowOpen()) ui.toggleDecodeWindow();
      const badge = document.getElementById('am-title-badge')!;
      const toggle = document.getElementById('am-sync-toggle') as HTMLButtonElement;
      const measurement = () => document.getElementById('am-input')!.textContent;
      const sequence = new DecodeSequence();
      const deliver = (...envelopes: DecodeEnvelope[]) => {
        Receiver.current.onmessage!({ data: { type: 'frames', envelopes } });
        ui.refreshDecodeWindow();
      };
      stream.setSdrIqEnabled(true);
      deliver(sequence.accept(frame({ mode: 'am' })), sequence.accept(frame({ mode: 'am', seq: 1,
        event: amEvent({ syncLocked: false, carrierHz: null, carrierOffsetHz: null, modulationPct: null, inputDbFs: null }) })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(measurement()).toBe('—');
      expect(document.getElementById('am-rms')!.textContent).toBe('-32.0 dBFS');
      deliver(sequence.accept(frame({ mode: 'am', seq: 2, event: { type: 'warning', message: 'Temporary AM sync warning' } })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(toggle.disabled).toBe(false);
      deliver(sequence.accept(frame({ mode: 'am', seq: 3, event: amEvent({ inputDbFs: 0 }) })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(true);
      expect(document.getElementById('am-carrier')!.textContent).toBe('9.810001 MHz');
      expect(measurement()).toBe('0.0 dBFS');
      deliver(sequence.accept(frame({ mode: 'am', seq: 4, event: { type: 'error', message: 'AM decoder failed' } })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(toggle.disabled).toBe(true);
      expect(measurement()).toBe('—');
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18 })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(toggle.disabled).toBe(false);
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18, seq: 1, event: amEvent() })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(true);
      stream.resetSdrIq();
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(measurement()).toBe('—');
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18, seq: 2, event: amEvent() })));
      expect(badge.classList.contains('on')).toBe(true);
      stream.setSdrIqEnabled(false);
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(measurement()).toBe('—');
      stream.setSdrIqEnabled(true);
      ui.confirmAmSyncGeneration(0);
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18, seq: 3, event: amEvent() })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(true);
      ui.confirmAmSyncGeneration(3);
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18, seq: 4, event: amEvent({ amSyncGeneration: 1 }) })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(measurement()).toBe('—');
      deliver(sequence.accept(frame({ mode: 'am', channel_id: 18, seq: 5, event: amEvent({ amSyncGeneration: 3 }) })));
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(true);
      const quiet = vi.spyOn(console, 'error').mockImplementation(() => {});
      try { Receiver.current.onmessage!({ data: { type: 'closed', message: 'Stream disconnected' } }); }
      finally { quiet.mockRestore(); }
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      expect(badge.classList.contains('on')).toBe(false);
      expect(toggle.disabled).toBe(true);
      expect(measurement()).toBe('—');
      stream.setSdrIqEnabled(false);
      stream.resetSdrIq();
      sdrAmSync.reset();
    } finally { vi.unstubAllGlobals(); document.body.innerHTML = ''; }
  });
  it('reports the original rejected stream error using a browser-valid close code', async () => {
    const messages: Array<{ type: string; message?: string }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; message?: string }) => messages.push(message) };
    let connection: { onmessage: ((event: { data: ArrayBuffer }) => void) | null } | null = null;
    const closed: number[] = [];
    class Socket {
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      constructor() { connection = this; }
      close(code: number): void {
        if (code !== 1000 && (code < 3000 || code > 4999)) throw new DOMException('Invalid close code', 'InvalidAccessError');
        closed.push(code);
      }
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker module initialization installs self.onmessage; browser globals must exist before import.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      const socket = connection! as Socket;
      socket.onmessage!({ data: frame({ seq: 1, event: { type: 'cw' } }) });
      expect(messages).toEqual([{ type: 'error', message: 'SDE2 identity or sequence gap' }]);
      expect(closed).toEqual([4002]);
      socket.onmessage!({ data: frame() });
      expect(messages).toHaveLength(1);
    } finally { vi.unstubAllGlobals(); }
  });
  it('stops on receiver failure without reconnecting and permits an explicit new start', async () => {
    const messages: Array<{ type: string; message?: string }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; message?: string }) => messages.push(message),
      setTimeout: vi.fn(), clearTimeout: vi.fn() };
    const sockets: Socket[] = [];
    class Socket {
      onclose: ((event: { code: number; reason: string }) => void) | null = null;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      constructor() { sockets.push(this); }
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker initialization binds self; load only after installing this worker's globals.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      sockets[0].onclose!({ code: 1011, reason: 'receiver failed' });
      sockets[0].onmessage!({ data: frame() });
      expect(messages).toEqual([{ type: 'error', message: 'Decoded-output receiver failed' }]);
      expect(host.setTimeout).not.toHaveBeenCalled();
      expect(sockets).toHaveLength(1);
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      sockets[1].onmessage!({ data: frame() });
      expect(messages[1].type).toBe('frames');
    } finally { vi.unstubAllGlobals(); }
  });
  it.each(['stop', 'error'])('clears in-flight delivery on %s and ignores the old socket callbacks', async (type) => {
    const messages: Array<{ type: string; envelopes?: DecodeEnvelope[] }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; envelopes?: DecodeEnvelope[] }) => messages.push(message),
      setTimeout: vi.fn(), clearTimeout: vi.fn() };
    class Socket {
      static instances: Socket[] = [];
      onopen: (() => void) | null = null;
      onerror: (() => void) | null = null;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      constructor() { Socket.instances.push(this); }
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker initialization must bind the installed worker globals.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      const old = Socket.instances[0];
      old.onmessage!({ data: frame() });
      host.onmessage!({ data: { type } });
      const count = messages.length;
      old.onopen!();
      old.onerror!();
      old.onmessage!({ data: frame({ seq: 1, event: { type: 'cw' } }) });
      expect(messages).toHaveLength(count);
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      Socket.instances[1].onmessage!({ data: frame({ channel_id: 18 }) });
      expect(messages.at(-1)?.envelopes?.[0].channel_id).toBe(18);
    } finally { vi.unstubAllGlobals(); }
  });
  it('acknowledges only host-applied sequences independently per channel', async () => {
    const messages: Array<{ type: string; envelopes?: DecodeEnvelope[] }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; envelopes?: DecodeEnvelope[] }) => messages.push(message) };
    class Socket {
      static current: Socket;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      sent: unknown[] = [];
      constructor(readonly url: string) { Socket.current = this; }
      send(text: string): void { this.sent.push(JSON.parse(text)); }
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker initialization binds self, which must be installed before this module loads.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode?channel=1' } });
      const socket = Socket.current;
      expect(socket.url).toBe('ws://localhost/ws/decode?channel=1&ack=1');
      socket.onmessage!({ data: frame({ event: { type: 'start', snapshotAck: true } }) });
      socket.onmessage!({ data: frame({ seq: 1, event: { type: 'cw' } }) });
      socket.onmessage!({ data: frame({ channel: 2, channel_id: 18, event: { type: 'start', snapshotAck: true } }) });
      socket.onmessage!({ data: frame({ seq: 2, event: { type: 'cw' } }) });
      expect(socket.sent).toEqual([]);
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent).toEqual([{ type: 'ack', channel: 1, epoch: 9, seq: 0 }]);
      expect(messages[1].envelopes?.map((item) => [item.channel, item.seq])).toEqual([[1, 1], [2, 0], [1, 2]]);
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent.slice(1)).toEqual([{ type: 'ack', channel: 1, epoch: 9, seq: 2 },
        { type: 'ack', channel: 2, epoch: 9, seq: 0 }]);
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent).toHaveLength(3);
    } finally { vi.unstubAllGlobals(); }
  });
  it.each(['wfm', 'am', 'cw'])('sends no wire ACK for an old %s start that omits snapshotAck', async (mode) => {
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null, postMessage: () => {} };
    class Socket {
      static current: Socket;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      sent: unknown[] = [];
      constructor() { Socket.current = this; }
      send(text: string): void { this.sent.push(JSON.parse(text)); }
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      const socket = Socket.current;
      const event = mode === 'am' ? amEvent() : mode === 'wfm' ? { type: 'wfm-info', peakDbFs: '-7.0' } : { type: 'cw', text: 'CQ' };
      socket.onmessage!({ data: frame({ mode }) });
      socket.onmessage!({ data: frame({ mode, seq: 1, event }) });
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent).toEqual([]);
    } finally { vi.unstubAllGlobals(); }
  });
  it('keeps wire ACK enabled after the initial advertisement even if a later start omits it', async () => {
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null, postMessage: () => {} };
    class Socket {
      static current: Socket;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      sent: unknown[] = [];
      constructor() { Socket.current = this; }
      send(text: string): void { this.sent.push(JSON.parse(text)); }
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      const socket = Socket.current;
      socket.onmessage!({ data: frame({ event: { type: 'start', snapshotAck: true } }) });
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent).toEqual([{ type: 'ack', channel: 1, epoch: 9, seq: 0 }]);
      socket.onmessage!({ data: frame({ channel_id: 18 }) });
      host.onmessage!({ data: { type: 'ack' } });
      socket.onmessage!({ data: frame({ channel_id: 18, seq: 1, event: { type: 'cw' } }) });
      host.onmessage!({ data: { type: 'ack' } });
      expect(socket.sent.slice(1)).toEqual([
        { type: 'ack', channel: 1, epoch: 9, seq: 0 },
        { type: 'ack', channel: 1, epoch: 9, seq: 1 },
      ]);
    } finally { vi.unstubAllGlobals(); }
  });
  it('rejects a non-boolean snapshotAck and accepts an explicit false without enabling the wire ACK', () => {
    const stream = new DecodeSequence();
    expect(() => stream.accept(frame({ event: { type: 'start', snapshotAck: 'true' as unknown as boolean } }))).toThrow(/snapshotAck/);
    expect(() => stream.accept(frame({ event: { type: 'start', snapshotAck: 1 as unknown as boolean } }))).toThrow(/snapshotAck/);
    expect(stream.accept(frame({ event: { type: 'start', snapshotAck: false } })).event.snapshotAck).toBe(false);
  });
  it('applies a retained 20 MiB radiofax snapshot with bounded outstanding transport credit', async () => {
    vi.resetModules();
    // Reset stream/store ownership to model a fresh browser before loading the worker host.
    const stream = await import('../sdr/iqStream');
    const store = await import('../sdr/radiofaxLog');
    const messages: Array<{ type: string; envelopes?: DecodeEnvelope[] }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; envelopes?: DecodeEnvelope[] }) => messages.push(message) };
    class Socket {
      static current: Socket;
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      applied: number | null = null;
      constructor() { Socket.current = this; }
      send(text: string): void {
        const ack = JSON.parse(text);
        expect(ack).toMatchObject({ type: 'ack', channel: 1, epoch: 9 });
        expect(typeof ack.seq).toBe('number');
        this.applied = ack.seq;
      }
      close(): void {}
    }
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker initialization binds the browser globals installed above.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      const socket = Socket.current;
      const apply = (packet: ArrayBuffer, seq: number): void => {
        expect(packet.byteLength).toBeLessThan(2 * 1024 * 1024);
        socket.onmessage!({ data: packet });
        expect(socket.applied).toBe(seq === 0 ? null : seq - 1);
        const delivered = messages.pop()!;
        expect(delivered.type).toBe('frames');
        for (const envelope of delivered.envelopes!) stream.consumeDecode(envelope);
        host.onmessage!({ data: { type: 'ack' } });
        expect(socket.applied).toBe(seq);
      };
      apply(frame({ mode: 'radiofax', event: { type: 'start', snapshotAck: true } }), 0);
      const pixels = btoa(String.fromCharCode(81).repeat(1810 * 64));
      let seq = 0;
      for (let segmentIndex = 0; segmentIndex < 5; segmentIndex++) {
        const lines = segmentIndex === 4 ? 1986 : 2400;
        for (let row = 0; row < lines; row += 64) {
          const rows = Math.min(64, lines - row);
          apply(frame({ mode: 'radiofax', seq: ++seq, event: { type: 'radiofax-image', width: 1810, row, rows,
            gray: rows === 64 ? pixels : btoa(String.fromCharCode(81).repeat(1810 * rows)), revision: 1,
            meta: { serial: String(segmentIndex), epoch: 'paper', hz: 9_110_000, ioc: 576, lpm: 120,
              lines: row + rows, complete: false, partial: false, segmentIndex, transmissionId: '1',
              missingRows: [], reason: null } } }), seq);
        }
      }
      expect(messages).toEqual([]);
      const page = store.radiofaxStatus().pages[0];
      expect(page.segments).toHaveLength(5);
      expect(page.lines).toBe(11586);
      expect(page.segments.reduce((bytes, segment) => bytes + segment.picture.gray.length, 0)).toBe(20_970_660);
      expect(store.radiofaxStatus().evicted).toBe(0);
    } finally { vi.unstubAllGlobals(); }
  }, 20_000);
  it('drains every queued decoded event in FIFO batches without waiting one round trip per event', async () => {
    const messages: Array<{ type: string; envelopes?: DecodeEnvelope[] }> = [];
    const host = { onmessage: null as ((event: { data: { type: string; url?: string } }) => void) | null,
      postMessage: (message: { type: string; envelopes?: DecodeEnvelope[] }) => messages.push(message) };
    let receive: ((event: { data: ArrayBuffer }) => void) | null = null;
    class Socket {
      set onmessage(callback: (event: { data: ArrayBuffer }) => void) { receive = callback; }
      send(): void {}
      close(): void {}
    }
    vi.resetModules();
    vi.stubGlobal('self', host);
    vi.stubGlobal('WebSocket', Socket);
    try {
      // Worker initialization binds self, so install browser globals before loading it.
      await import('../sdr/decodeWorker');
      host.onmessage!({ data: { type: 'start', url: 'ws://localhost/ws/decode' } });
      (receive! as (event: { data: ArrayBuffer }) => void)({ data: frame() });
      for (let seq = 1; seq <= 20; seq++) {
        (receive! as (event: { data: ArrayBuffer }) => void)({ data: frame({ seq, event: { type: 'cw', text: String(seq) } }) });
      }
      host.onmessage!({ data: { type: 'ack' } });
      expect(messages.map((message) => message.envelopes?.map((event) => event.seq))).toEqual([[0], Array.from({ length: 20 }, (_, index) => index + 1)]);
      expect(messages[1].envelopes?.map((event) => event.event.text)).toEqual(Array.from({ length: 20 }, (_, index) => String(index + 1)));
      // The twenty unacknowledged events plus twelve queued events fill the
      // same ceiling; batching must not create another queue's worth of credit.
      for (let seq = 21; seq <= 32; seq++) {
        (receive! as (event: { data: ArrayBuffer }) => void)({ data: frame({ seq, event: { type: 'cw', text: String(seq) } }) });
      }
      expect(messages).toHaveLength(2);
      (receive! as (event: { data: ArrayBuffer }) => void)({ data: frame({ seq: 33, event: { type: 'cw', text: '33' } }) });
      expect(messages[2]).toEqual({ type: 'error', message: 'Slow decoded-output consumer: queue exceeded' });
    } finally { vi.unstubAllGlobals(); }
  });
});
