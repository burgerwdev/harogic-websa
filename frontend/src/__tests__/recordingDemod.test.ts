import { afterEach, describe, expect, it, vi } from 'vitest';
import { demodWav, mountRecordingDemod } from '../ui/recordingDemod';
import type { RecordingTask } from '../core/multichannel';

afterEach(() => { document.body.innerHTML = ''; vi.unstubAllGlobals(); });
describe('offline recording processing lifecycle', () => {
  it.each(['missing length', 'excess streamed bytes'])('cancels file rendering responses with %s before buffering an unbounded body', async scenario => {
    const body = document.createElement('div'); document.body.append(body);
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const cancel = vi.fn();
    const stream = new ReadableStream<Uint8Array>({ start(value) { controller = value; value.enqueue(new Uint8Array(1025)); }, cancel });
    const headers = scenario === 'missing length' ? undefined : { 'Content-Length': '1024' };
    vi.stubGlobal('fetch', vi.fn(async () => new Response(stream, { headers })));
    const dispose = mountRecordingDemod(body, { task: { task_id: 'bounded-render' } as RecordingTask, modes: ['am'],
      selection: () => ({ query: new URLSearchParams({ segment: 's', start: '0', count: '2' }),
        segment: { samples: '2', actual_rate_hz: 6000 } as RecordingTask['tracks'][number]['segments'][number] }) });
    try {
      body.querySelector<HTMLButtonElement>('[data-demod-run]')!.click();
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(cancel).toHaveBeenCalledOnce();
      expect(stream.locked).toBe(false);
      expect(body.querySelector<HTMLButtonElement>('[data-demod-run]')!.disabled).toBe(false);
      expect(body.querySelector<HTMLAudioElement>('audio')!.hasAttribute('src')).toBe(false);
    } finally { dispose(); if (!cancel.mock.calls.length) controller.close(); }
  });

  it('hides file DSP on an older service and cancels old range requests without reviving a disposed player', async () => {
    const body = document.createElement('div'); document.body.append(body);
    const selection = () => ({ query: new URLSearchParams({ segment: 'old-stream', start: '9007199254740993', count: '1000' }),
      segment: { samples: '18446744073709551615', actual_rate_hz: 6000 } as RecordingTask['tracks'][number]['segments'][number] });
    const task = { task_id: 'old-task' } as RecordingTask;
    mountRecordingDemod(body, { task, modes: [], selection }); expect(body.children.length).toBe(0);
    let resolve!: (r: Response) => void;
    const fetch = vi.fn((_url: string, _options: RequestInit) => new Promise<Response>(r => { resolve = r; })); vi.stubGlobal('fetch', fetch);
    const dispose = mountRecordingDemod(body, { task, modes: ['am'], selection });
    expect(body.querySelector<HTMLElement>('[data-demod-offset-field]')!.hidden).toBe(true);
    body.querySelector<HTMLButtonElement>('[data-demod-run]')!.click();
    expect(fetch.mock.calls[0][0]).toContain('start=9007199254740993');
    expect(fetch.mock.calls[0][0]).not.toContain('offset_hz');
    const signal = (fetch.mock.calls[0] as unknown as [string, RequestInit])[1].signal!;
    expect(signal.aborted).toBe(false);
    dispose(); expect(signal.aborted).toBe(true);
    resolve({ ok: true, headers: new Headers(), arrayBuffer: async () => new ArrayBuffer(0) } as Response);
    await Promise.resolve(); await Promise.resolve(); expect(body.children.length).toBe(0);
  });
  it('sends signed fractional file tuning only to capable services and cancels an obsolete offset', async () => {
    const body = document.createElement('div'); document.body.append(body);
    const fetch = vi.fn((_url: string, _options: RequestInit) => new Promise<Response>(() => {})); vi.stubGlobal('fetch', fetch);
    const dispose = mountRecordingDemod(body, { task: { task_id: 'old-task' } as RecordingTask, modes: ['usb'], frequencyOffset: true,
      selection: () => ({ query: new URLSearchParams({ segment: 'old-stream', start: '9007199254740993', count: '20000' }),
        segment: { actual_rate_hz: 48000, frequency_hz: 100e6 } as RecordingTask['tracks'][number]['segments'][number] }) });
    const offset = body.querySelector<HTMLInputElement>('[data-demod-offset]')!;
    offset.value = '-2000.25'; offset.dispatchEvent(new Event('input', { bubbles: true }));
    expect(body.querySelector('[data-demod-frequency]')!.textContent).toBe('99,997,999.75');
    body.querySelector<HTMLButtonElement>('[data-demod-run]')!.click();
    const query = new URL(fetch.mock.calls[0][0], 'http://localhost').searchParams;
    expect(query.get('offset_hz')).toBe('-2000.25'); expect(query.get('start')).toBe('9007199254740993');
    const signal = fetch.mock.calls[0][1].signal!;
    offset.value = '0.001'; offset.dispatchEvent(new Event('input', { bubbles: true }));
    expect(signal.aborted).toBe(true);
    expect(body.querySelector('[data-demod-offset]')).toBe(offset);
    expect(body.querySelector('[data-demod-frequency]')!.textContent).toBe('100,000,000.001');
    offset.value = ''; offset.dispatchEvent(new Event('input', { bubbles: true }));
    body.querySelector<HTMLButtonElement>('[data-demod-run]')!.click(); await Promise.resolve();
    expect(fetch).toHaveBeenCalledTimes(1); expect(body.querySelector('[data-demod-status]')!.textContent).toContain('offset');
    dispose();
  });
  it('rejects overlong selections before a server request and preserves live controls', async () => {
    const body = document.createElement('div'); document.body.append(body);
    const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
    const dispose = mountRecordingDemod(body, { task: { task_id: 't' } as RecordingTask, modes: ['am'],
      selection: () => ({ query: new URLSearchParams({ count: '1048576' }), segment: { actual_rate_hz: 6000 } as RecordingTask['tracks'][number]['segments'][number] }) });
    body.querySelector<HTMLButtonElement>('[data-demod-run]')!.click(); await Promise.resolve();
    expect(fetch).not.toHaveBeenCalled(); expect(body.querySelector('[data-demod-status]')!.textContent).toContain('60');
    expect(body.querySelector<HTMLButtonElement>('[data-demod-run]')!.disabled).toBe(false); dispose();
  });
  it('rejects truncated, oversized and path-bearing archives before creating playback URLs', () => {
    expect(() => demodWav(new ArrayBuffer(0))).toThrow();
    expect(() => demodWav(new ArrayBuffer(20 * 1024 * 1024 + 1))).toThrow();
    const bytes = new Uint8Array(2048); bytes.set(new TextEncoder().encode('../audio.wav'));
    bytes.set(new TextEncoder().encode('00000000004'), 124);
    expect(() => demodWav(bytes.buffer)).toThrow();
  });
});
