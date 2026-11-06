import { afterEach, describe, expect, it } from 'vitest';
import { consumeReceiverResult } from '../ui/nativeWorkbenchStreams';
import { decoderTextLine, decoderLog, forgetDecoderLog } from '../sdr/decoderLog';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';
const envelope = (event: DecodeEnvelope['event'], mode = 'ft8', epoch = 8): DecodeEnvelope => ({ version: 1, channel: 1,
  channel_id: 8, epoch, seq: 1, input_index: '9007199254740999', input_fraction_num: '1', input_fraction_den: '3',
  input_rate: 62500000, delay_seconds: 0.01, delay_known: true, causal_presentation: false, mode, event });
afterEach(() => { forgetDecoderLog('uid-a'); forgetDecoderLog('uid-b'); });
describe('UID receiver result histories', () => {
  it('retires live telemetry without exposing a processing-end row or discarding decoded text', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: 'RETAINED', rms: 0.2 }, 'cw'));
    consumeReceiverResult('uid-b', envelope({ type: 'cw', text: 'NEIGHBOR', rms: 0.3 }, 'cw'));
    consumeReceiverResult('uid-a', envelope({ type: 'processing-end', processing_generation: '4',
      drained: false, tail_complete: true, processing_complete: false, reason: 'reconfigured' }, 'cw'));
    expect(decoderLog('uid-a').entries).toHaveLength(1);
    expect(decoderTextLine(decoderLog('uid-a').entries[0])?.text).toBe('RETAINED');
    expect(decoderLog('uid-b').entries).toHaveLength(2);
    consumeReceiverResult('uid-a', envelope({ type: 'start' }, 'ft8', 9));
    consumeReceiverResult('uid-a', envelope({ type: 'ft8', text: 'CQ W1ABC FN42', centerHz: 14_074_000 }, 'ft8', 9));
    expect(decoderLog('uid-a').entries[0]).toMatchObject({ mode: 'ft8', kind: 'table' });
  });
  it('combines DRM metadata, heartbeat and constellation without crossing receiver or stream boundaries', () => {
    const points = [{ re: 0.7, im: -0.7 }];
    consumeReceiverResult('uid-a', envelope({ type: 'drm', lines: ['station: Radio', 'FAC MER 22.5 dB'], snrDb: 18 }, 'drm'));
    consumeReceiverResult('uid-a', envelope({ type: 'drm-constellation', points }, 'drm'));
    consumeReceiverResult('uid-a', envelope({ type: 'drm', lines: [] }, 'drm'));
    expect(decoderLog('uid-a').entries).toHaveLength(1);
    expect(decoderLog('uid-a').entries[0].data).toMatchObject({ type: 'drm', points, snrDb: 18, lines: ['station: Radio', 'FAC MER 22.5 dB'] });
    consumeReceiverResult('uid-b', envelope({ type: 'drm-constellation', points }, 'drm'));
    expect(decoderLog('uid-b').entries[0].data).not.toHaveProperty('snrDb', 18);
    consumeReceiverResult('uid-a', envelope({ type: 'drm', lines: ['DRM searching: mode A'] }, 'drm', 9));
    expect(decoderLog('uid-a').entries[0].data).toMatchObject({ points: [], snrDb: null, lines: ['DRM searching: mode A'] });
  });
  it('retains CW live telemetry separately from incremental decoded text', () => {
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: 'CQ', rms: 0.1, wpm: 18, pitchHz: 700, afcHz: 2 }, 'cw'));
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: '', rms: 0.2, wpm: 19, pitchHz: 701, afcHz: 3 }, 'cw'));
    const log = decoderLog('uid-a');
    expect(log.entries.filter(e => e.kind === 'text').map(e => decoderTextLine(e)?.text)).toEqual(['CQ']);
    expect(log.entries.filter(e => e.kind === 'status')).toHaveLength(1);
    expect(log.entries[0].data).toMatchObject({ type: 'cw-level', rms: 0.2, wpm: 19 });
    expect(log.entries[0].timing?.input_index).toBe('9007199254740999');
  });
  it('retains CW transmissions rather than characters and honors empty end-of-line events and new streams', () => {
    for (const text of 'CQ DE TEST '.repeat(30)) consumeReceiverResult('uid-a', envelope({ type: 'cw', text, share: 0.8 }, 'cw'));
    let lines = decoderLog('uid-a').entries.filter(e => e.kind === 'text');
    expect(lines).toHaveLength(1); expect(decoderTextLine(lines[0])?.text).toBe('CQ DE TEST '.repeat(30));
    expect(decoderLog('uid-a').evicted).toBe(0);
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: '', endsLine: true }, 'cw'));
    expect(decoderTextLine(decoderLog('uid-a').entries[0])?.endsLine).toBe(true);
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: 'NEXT', share: 0.2 }, 'cw'));
    consumeReceiverResult('uid-b', envelope({ type: 'cw', text: 'OTHER' }, 'cw'));
    consumeReceiverResult('uid-a', envelope({ type: 'cw', text: 'NEW STREAM' }, 'cw', 9));
    lines = decoderLog('uid-a').entries.filter(e => e.kind === 'text');
    expect(lines.map(e => decoderTextLine(e)?.text)).toEqual(['NEW STREAM', 'NEXT', 'CQ DE TEST '.repeat(30) + '\n']);
    expect(decoderTextLine(lines[1])?.marks[0].share).toBe(0.2);
    expect(decoderLog('uid-b').entries.filter(e => e.kind === 'text')).toHaveLength(1);
  });
  it('keeps only the current RTTY telemetry snapshot while retaining its continuous text once', () => {
    for (const characters of [1, 2, 3]) {
      consumeReceiverResult('uid-a', envelope({ type: 'decoder-result', provider: 'onair-rtty', kind: 'text', data: { text: 'CQ '[characters - 1], characters } }, 'rtty'));
      consumeReceiverResult('uid-a', envelope({ type: 'decoder-result', provider: 'onair-rtty', kind: 'table', data: { state: 'receiving', characters, text: 'CQ '.slice(0, characters), baud: 45.45, quality: 0.8 } }, 'rtty'));
    }
    const entries = decoderLog('uid-a').entries;
    expect(entries).toHaveLength(2);
    expect(entries.find(entry => entry.kind === 'text')?.data).toMatchObject({ text: 'CQ ', characters: 3 });
    expect(entries.find(entry => entry.kind === 'status')?.data).toMatchObject({ type: 'rtty-state', characters: 3 });
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
    expect(decoderLog('uid-b').entries[0]).toMatchObject({ provider: 'onair-rtty', data: { text: 'HELLO' }, source: '8:8' });
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
