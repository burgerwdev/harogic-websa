import { describe, expect, it } from 'vitest';
import { pictureExport, pictureIsPartial, resultExport } from '../ui/receiverExport';
import type { DecoderImage, DecoderLogEntry, DecoderRaster } from '../sdr/decoderLog';

const entry: DecoderLogEntry = { id: 1, at: 1793500000000, provider: 'native', source: '8:9', mode: 'ft8', kind: 'table', bytes: 64,
  timing: { input_index: '9007199254740999', fraction_num: '1', fraction_den: '3', input_rate: 62500000, delay_seconds: 0.01, delay_known: true, causal_presentation: false },
  data: { decodes: [{ msg: 'CQ "TEST", AA00', snr_db: -12, audio_hz: 1099, dt_s: 0.25 }] } };

describe('retained receiver result downloads', () => {
  it('identifies lost image prefixes, missing bands and interrupted pictures even after a completion event', () => {
    const raster: DecoderRaster = { raster: true, id: 'a', label: 'Fax', w: 2, h: 2, rows: 2, rgb: new Uint8Array(12), final: true, partial: false, missingRows: [] };
    expect(pictureIsPartial(raster)).toBe(false);
    expect(pictureIsPartial({ ...raster, missingRows: [1] })).toBe(true);
    expect(pictureIsPartial({ ...raster, final: false })).toBe(true);
    const image: DecoderImage = { id: 'b', label: 'Satellite', w: 1, h: 3, final: true,
      strips: [{ png: '', y: 1, rows: 2, pw: 1, ph: 2 }] };
    expect(pictureIsPartial(image)).toBe(true);
    expect(pictureIsPartial({ ...image, strips: [{ png: '', y: 0, rows: 3, pw: 1, ph: 3 }] })).toBe(false);
  });
  it('rejects a large external picture before allocating an export canvas', async () => {
    const oversized = { ...entry, kind: 'image' as const, data: { id: 'a', w: 8192, h: 32768, final: true,
      label: 'Large', strips: [{ png: '', y: 0, rows: 32768, pw: 8192, ph: 32768 }] } };
    await expect(pictureExport(document, oversized)).rejects.toThrow('20 MiB');
  });
  it('preserves precise source timing and receipt time without inventing protocol UTC in JSONL', () => {
    const data = JSON.parse(resultExport([entry], 'service:uid', 'jsonl'));
    expect(data).toMatchObject({ identity: 'service:uid', received_at_ms: entry.at, time_basis: 'browser_receipt', source: '8:9',
      timing: { input_index: '9007199254740999', fraction_num: '1', fraction_den: '3' }, data: entry.data });
    expect(data.data.decodes[0]).not.toHaveProperty('slot_utc');
  });
  it('exports all message pages, CSV escaping and numeric negative measurements', () => {
    const data = resultExport(Array.from({ length: 60 }, (_, i) => ({ ...entry, id: i + 1 })), 'service:uid', 'csv');
    expect(data.split('\r\n')).toHaveLength(62);
    expect(data).toContain('"-12","0.25","1099","CQ ""TEST"", AA00"');
    expect(data).not.toContain("'-12");
    expect(data).toContain('"received_at_utc","slot_utc"');
  });
  it('keeps text that could be interpreted as a spreadsheet formula inert and omits live status', () => {
    const formula = { ...entry, data: { decodes: [{ msg: '=1+1' }] } };
    expect(resultExport([formula], 'uid', 'csv')).toContain('"\'=1+1"');
    const status = { ...entry, kind: 'status' as const, data: { type: 'ft8-state' } };
    expect(resultExport([entry, status], 'uid', 'jsonl').trim().split('\n')).toHaveLength(1);
  });
});
