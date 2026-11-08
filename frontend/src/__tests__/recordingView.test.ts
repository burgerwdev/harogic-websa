import { describe, expect, it, vi } from 'vitest';
import { mountRecordingView, recordingRange } from '../ui/recordingView';
import type { RecordingTask } from '../core/multichannel';

describe('recorded IQ sample selection', () => {
  const smallTask = (): RecordingTask => ({ task_id: 'bounded-iq', receiver_uid: 'old-uid', state: 'completed', fault: null,
    tracks: [{ track_id: 'iq', kind: 'iq', format: 'cf32_le', source: 'baseband', compression: 'zstd_auto_lossless',
      state: 'completed', raw_bytes: '16', stored_bytes: '16', segments: [{ segment_id: 'segment-a', samples: '2', actual_rate_hz: 6000,
        first_input_index: '9007199254740993', stream: { source_id: 'archive', epoch: '1', channel_id: '7' }, end_reason: 'stopped' }] }] });

  it.each(['waveform', 'SigMF'])('rejects an oversized %s response before consuming its body', async target => {
    const cancel = vi.fn(), pull = vi.fn((controller: ReadableStreamDefaultController<Uint8Array>) => {
      controller.enqueue(new Uint8Array(16)); controller.close();
    });
    const body = new ReadableStream<Uint8Array>({ pull, cancel }, { highWaterMark: 0 });
    vi.stubGlobal('fetch', vi.fn(async () => new Response(body, { headers: { 'Content-Length': String(16 * 1024 * 1024 + 1) } })));
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    const view = mountRecordingView();
    try {
      view.open(smallTask());
      if (target === 'waveform') document.querySelector<HTMLFormElement>('[data-range-form]')!.requestSubmit();
      else document.querySelector<HTMLButtonElement>('[data-download]')!.click();
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(pull).not.toHaveBeenCalled();
      expect(cancel).toHaveBeenCalledOnce();
      expect(body.locked).toBe(false);
      expect(document.querySelector<HTMLButtonElement>('[data-load]')!.disabled).toBe(false);
      expect(document.querySelector<HTMLButtonElement>('[data-play]')!.disabled).toBe(true);
    } finally { view.dispose(); canvas.mockRestore(); vi.unstubAllGlobals(); }
  });

  it('cancels an IQ body as soon as streamed bytes exceed the declared selection size', async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const cancel = vi.fn();
    const body = new ReadableStream<Uint8Array>({ start(value) { controller = value; value.enqueue(new Uint8Array(17)); }, cancel });
    const fetch = vi.fn(async () => new Response(body, { headers: { 'Content-Length': '16' } })); vi.stubGlobal('fetch', fetch);
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    const view = mountRecordingView();
    try {
      view.open(smallTask()); document.querySelector<HTMLFormElement>('[data-range-form]')!.requestSubmit();
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(cancel).toHaveBeenCalledOnce();
      expect(body.locked).toBe(false);
      expect(document.querySelector<HTMLButtonElement>('[data-load]')!.disabled).toBe(false);
      expect(document.querySelector<HTMLButtonElement>('[data-play]')!.disabled).toBe(true);
      const samples = new Uint8Array(new Float32Array([0.5, -0.25, 0.125, 0.75]).buffer);
      fetch.mockResolvedValueOnce(new Response(new ReadableStream({ start(value) {
        value.enqueue(samples.slice(0, 5)); value.enqueue(samples.slice(5)); value.close();
      } }), { headers: { 'Content-Length': '16' } }));
      document.querySelector<HTMLFormElement>('[data-range-form]')!.requestSubmit();
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(document.querySelector<HTMLButtonElement>('[data-play]')!.disabled).toBe(false);
      expect(document.querySelector('[data-info]')!.textContent).toContain('2 samples');
    } finally { view.dispose(); if (!cancel.mock.calls.length) controller.close(); canvas.mockRestore(); vi.unstubAllGlobals(); }
  });

  it('gates collection downloads on the service capability and uses native delivery independently of the range form', () => {
    const task: RecordingTask = { task_id: 'old-task', receiver_uid: 'old-uid', service_id: 'old-service', state: 'completed', fault: null,
      tracks: [{ track_id: 'iq', kind: 'iq', format: 'cf32_le', source: 'baseband', compression: 'zstd_auto_lossless', state: 'completed',
        raw_bytes: '800', stored_bytes: '800', segments: ['a', 'b'].map(segment_id => ({ segment_id, samples: '50', first_input_index: '9007199254740993',
          actual_rate_hz: 6000, stream: { source_id: 'primary', epoch: '1', channel_id: segment_id }, end_reason: 'reconfigured' })) }] };
    const old = mountRecordingView(); old.open(task);
    expect(document.querySelector<HTMLButtonElement>('[data-download-collection]')!.hidden).toBe(true);
    old.dispose();
    const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
    let endpoint = '';
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) { endpoint = this.href; });
    const view = mountRecordingView({ collection: () => true });
    try {
      view.open(task);
      expect(document.querySelector<HTMLButtonElement>('[data-download-collection]')!.hidden).toBe(false);
      document.querySelector<HTMLInputElement>('[name=count]')!.value = 'invalid-range';
      document.querySelector<HTMLButtonElement>('[data-download-collection]')!.click();
      expect(endpoint).toContain('/api/sdr/recordings/old-task/collection');
      expect(endpoint).not.toContain('?'); expect(fetch).not.toHaveBeenCalled();
    } finally { view.dispose(); click.mockRestore(); vi.unstubAllGlobals(); }
  });
  it('keeps large downloads separate from the bounded waveform preview', () => {
    expect(() => recordingRange('3000000', '0', '2000000')).toThrow();
    expect(recordingRange('3000000', '0', '2000000', 1073741823n).get('count')).toBe('2000000');
    expect(() => recordingRange('3000000000', '0', '1073741824', 1073741823n)).toThrow();
  });
  it('keeps range offsets beyond Number precision as exact decimal strings', () => {
    const query = recordingRange('18446744073709551615', '9007199254740993', '1048576');
    expect(query.get('start')).toBe('9007199254740993');
    expect(query.get('count')).toBe('1048576');
  });
  it('rejects empty, fractional, negative, oversized and overflowing segment ranges', () => {
    for (const [start, count] of [['', '1'], ['0.5', '1'], ['-1', '1'], ['0', '0'], ['0', '1048577'], ['100', '1']]) {
      expect(() => recordingRange('100', start, count)).toThrow();
    }
  });
  it('allows the final sample without crossing the segment boundary', () => {
    expect(recordingRange('9007199254740995', '9007199254740994', '1').get('start')).toBe('9007199254740994');
    expect(() => recordingRange('9007199254740995', '9007199254740994', '2')).toThrow();
  });
});
