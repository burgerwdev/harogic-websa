import { describe, expect, it, vi } from 'vitest';
import { mountRecordingView, recordingRange } from '../ui/recordingView';
import type { RecordingTask } from '../core/multichannel';

describe('recorded IQ sample selection', () => {
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
