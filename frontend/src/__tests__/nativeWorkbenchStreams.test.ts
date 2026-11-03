import { afterEach, describe, expect, it } from 'vitest';
import { consumeReceiverResult } from '../ui/nativeWorkbenchStreams';
import { decoderLog, forgetDecoderLog } from '../sdr/decoderLog';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';
const envelope = (event: DecodeEnvelope['event'], mode = 'ft8', epoch = 8): DecodeEnvelope => ({ version: 1, channel: 1,
  channel_id: 8, epoch, seq: 1, input_index: '9007199254740999', input_fraction_num: '1', input_fraction_den: '3',
  input_rate: 62500000, delay_seconds: 0.01, delay_known: true, causal_presentation: false, mode, event });
afterEach(() => { forgetDecoderLog('uid-a'); forgetDecoderLog('uid-b'); });
describe('UID receiver result histories', () => {
  it('retains CW live telemetry separately from incremental decoded text', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: 'CQ', rms: 0.1, wpm: 18, pitchHz: 700, afcHz: 2 }, 'cw'));
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: '', rms: 0.2, wpm: 19, pitchHz: 701, afcHz: 3 }, 'cw'));
    const log = decoderLog('uid-a');
    expect(log.entries.filter(e => e.kind === 'text').map(e => e.data)).toEqual(['CQ']);
    expect(log.entries.filter(e => e.kind === 'status')).toHaveLength(1);
    expect(log.entries[0].data).toMatchObject({ type: 'cw-level', rms: 0.2, wpm: 19 });
    expect(log.entries[0].timing?.input_index).toBe('9007199254740999');
  });
  it('clears old live readings and errors on a new processing stream while retaining decoded messages', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'ft8', text: 'CQ KEEP AA00' }));
    consumeReceiverResult('uid-a', envelope({ type: 'ft8-state', bufferedSeconds: 7.5 }));
    consumeReceiverResult('uid-a', envelope({ type: 'error', message: 'Old decoder failure' }));
    expect(decoderLog('uid-a').entries).toHaveLength(3);
    consumeReceiverResult('uid-a', envelope({ type: 'start' }, 'ft8', 9));
    expect(decoderLog('uid-a').entries).toHaveLength(1);
    expect(decoderLog('uid-a').entries[0].data).toMatchObject({ decodes: [{ msg: 'CQ KEEP AA00' }] });
  });
  it('keeps actual FT8 reports and provider text isolated from selection and other receivers', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'ft8', text: 'CQ W1ABC FN42', snrDb: -11, timeOffsetS: 0.25, frequencyHz: 1099, centerHz: 14074000 }));
    consumeReceiverResult('uid-b', envelope({ type: 'decoder-result', provider: 'onair-rtty', mode: 'rtty', kind: 'text', data: 'HELLO' }, 'rtty'));
    expect(decoderLog('uid-a').entries[0].data).toEqual({ decodes: [{ snr_db: -11, dt_s: 0.25, audio_hz: 1099, msg: 'CQ W1ABC FN42', center_hz: 14074000, window_start: '9007199254740999' }] });
    expect(decoderLog('uid-b').entries[0]).toMatchObject({ provider: 'onair-rtty', data: 'HELLO', source: '8:8' });
    consumeReceiverResult('uid-a', envelope({ type: 'start' }, 'ft8', 9));
    expect(decoderLog('uid-a').entries).toHaveLength(1); expect(decoderLog('uid-b').entries).toHaveLength(1);
  });
  it('assembles native RGB and gray rows while retaining partial old pictures across boundaries', () => {
    const row = (serial: string, row: number, rgb: number[], complete = false) => envelope({ type: 'sstv-image', width: 2, height: 2,
      row, rows: 1, rgb, meta: { serial, name: 'Robot 36', complete } }, 'sstv');
    consumeReceiverResult('uid-a', row('1', 0, [255, 0, 0, 0, 255, 0]));
    consumeReceiverResult('uid-a', row('1', 1, [0, 0, 255, 255, 255, 255], true));
    let picture = decoderLog('uid-a').entries[0].data as { rgb: Uint8Array; final: boolean };
    expect([...picture.rgb]).toEqual([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]); expect(picture.final).toBe(true);
    consumeReceiverResult('uid-a', envelope({ type: 'start' }, 'radiofax', 9));
    for (let row = 0; row < 2; row++) consumeReceiverResult('uid-a', envelope({ type: 'radiofax-image', width: 2, row, rows: 1,
      gray: btoa(String.fromCharCode(row * 100, 128)), meta: { serial: 'fax', lines: row + 1, partial: true } }, 'radiofax', 9));
    picture = decoderLog('uid-a').entries[0].data as typeof picture;
    expect([...picture.rgb]).toEqual([0, 0, 0, 128, 128, 128, 100, 100, 100, 128, 128, 128]);
    expect(decoderLog('uid-a').entries).toHaveLength(2); expect(decoderLog('uid-b').entries).toHaveLength(0);
  });
  it('rejects malformed native pixels and keeps the previous image', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'sstv-image', width: 1, height: 1, row: 0, rows: 1, rgb: [1, 2, 3], meta: { serial: '1' } }, 'sstv'));
    expect(() => consumeReceiverResult('uid-a', envelope({ type: 'sstv-image', width: 1, height: 1, row: 0, rows: 1, rgb: [255], meta: { serial: '1' } }, 'sstv'))).toThrow('Invalid native image rows');
    expect([...(decoderLog('uid-a').entries[0].data as { rgb: Uint8Array }).rgb]).toEqual([1, 2, 3]);
  });
});
