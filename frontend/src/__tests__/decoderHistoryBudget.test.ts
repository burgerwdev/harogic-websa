import { afterEach, describe, expect, it } from 'vitest';
import { acknowledgeDecoderLog, addDecoderRaster, addDecoderResult, decoderHistoryUsage, decoderLog,
  DECODER_HISTORY_BYTES, endDecoderRasters, forgetDecoderLog } from '../sdr/decoderLog';

const uids = Array.from({ length: 8 }, (_, index) => `budget-receiver-${index}`);
afterEach(() => { for (const uid of uids) forgetDecoderLog(uid); });
describe('global receiver result history', () => {
  it('evicts acknowledged oldest history across eight receivers within the total budget', () => {
    const message = 'x'.repeat(1024 * 1024);
    for (const uid of uids) for (let index = 0; index < 10; index++) {
      addDecoderResult({ provider: 'native', mode: 'ft8', kind: 'text', data: { message, index } }, uid, uid);
      acknowledgeDecoderLog(uid);
      expect(decoderHistoryUsage().bytes).toBeLessThanOrEqual(DECODER_HISTORY_BYTES);
    }
    expect(decoderLog(uids[0]).evicted).toBeGreaterThan(0);
    expect(decoderLog(uids[7]).entries).toHaveLength(10);
    const before = decoderHistoryUsage().bytes;
    forgetDecoderLog(uids[7]); expect(decoderHistoryUsage().bytes).toBeLessThan(before);
  });
  it('refuses an image before retention can erase an active picture or falsely confirm it complete', () => {
    const image = { id: 'picture', label: 'Live page', w: 2048, h: 3400, row: 0, rows: 0,
      rgb: new Uint8Array(0), final: false, partial: false };
    for (const uid of uids.slice(0, 6)) { addDecoderRaster(uid, 'radiofax', uid, image); acknowledgeDecoderLog(uid); }
    expect(() => addDecoderRaster(uids[6], 'radiofax', uids[6], image)).toThrow(/budget.*incomplete/);
    expect(decoderLog(uids[6]).entries).toHaveLength(0);
    expect(decoderHistoryUsage().bytes).toBeLessThanOrEqual(DECODER_HISTORY_BYTES);
    endDecoderRasters(uids[0]);
    addDecoderRaster(uids[6], 'radiofax', uids[6], image);
    expect(decoderLog(uids[0]).evicted).toBe(1);
    expect(decoderLog(uids[6]).entries).toHaveLength(1);
  });
});
