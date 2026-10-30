import { beforeEach, describe, expect, it } from 'vitest';
import { addDecoderResult, clearDecoderLog, decoderLog, DECODER_LOG_ENTRIES } from '../sdr/decoderLog';
import { renderDecoderResults } from '../ui/decoderResults';

beforeEach(() => { clearDecoderLog(1); document.body.replaceChildren(); });
describe('provider result history', () => {
  it('puts vessel identity before telemetry regardless of upstream JSON key order', () => {
    addDecoderResult({ provider: 'onair', mode: 'ais', kind: 'table', data: { vessels: [{
      age_s: 1, cog_deg: 45, lat: 55, lon: 21, mmsi: 123456789, msgs: 8, name: 'VESSEL', sog_kt: 12,
    }] } }, '1:1', 1);
    const host = document.createElement('section');
    renderDecoderResults(host, 1, 'ais', false);
    expect([...host.querySelectorAll('th')].slice(0, 6).map(cell => cell.textContent))
      .toEqual(['name', 'MMSI', 'lat', 'lon', 'sog kt', 'cog deg']);
    expect(host.querySelectorAll('td')[1].textContent).toBe('123456789');
  });
  it('accepts the upstream 1568 by 12000 Meteor picture without allocating a full canvas', () => {
    const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'image', data: {
      id: 1, label: 'Meteor', w: 1568, h: 12000, ch: 1, y: 208, rows: 16,
      pw: 1, ph: 1, png, preview: true, scale: 1568,
    } }, '1:1', 1);
    const host = document.createElement('section');
    renderDecoderResults(host, 1, 'meteor', false);
    expect(host.querySelector<HTMLElement>('.decoder-image')?.style.aspectRatio).toBe('1568 / 224');
    expect(host.querySelector('strong')?.textContent).toContain('224 / 12000');
    expect(Number.parseFloat(host.querySelector('img')!.style.top)).toBeCloseTo(208 / 224 * 100);
    expect(Number.parseFloat(host.querySelector('img')!.style.height)).toBeCloseTo(16 / 224 * 100);
    expect(decoderLog(1, 'meteor').entries[0].data).toMatchObject({ h: 12000 });
    expect(host.querySelectorAll('img')).toHaveLength(1);
    expect(host.querySelector('canvas')).toBeNull();
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'status', data: {
      state: 'receiving', lines: 224, snr_db: 12, packets_ok: 125, frames_bad: 4, diagnostic: 'verbose',
    } }, '1:1', 1);
    renderDecoderResults(host, 1, 'meteor', false);
    expect(host.querySelector('.decoder-result')?.classList.contains('decoder-result-image')).toBe(true);
    expect(host.querySelector('.decoder-result-status dl')?.textContent).toContain('receiving');
    expect(host.querySelector('.decoder-result-status dl')?.textContent).not.toContain('diagnostic');
    expect(host.querySelector('.decoder-result-status details')).not.toBeNull();
  });
  it('bounds text while preserving per-mode clear and real message content', () => {
    addDecoderResult({ provider: 'rtl433', mode: 'ism', kind: 'table', data: { model: 'Weather', temperature_C: 21.5 } }, '1:1', 1);
    for (let index = 0; index < DECODER_LOG_ENTRIES; index++) {
      addDecoderResult({ provider: 'multimon', mode: 'pocsag1200', kind: 'text', data: { text: `Address ${index}: message` } }, '1:2', 1);
    }
    expect(decoderLog(1, 'ism').entries).toHaveLength(0);
    expect(decoderLog(1, 'pocsag1200').entries).toHaveLength(DECODER_LOG_ENTRIES);
    expect(decoderLog(1, 'pocsag1200').evicted).toBe(1);
    clearDecoderLog(1, 'pocsag1200');
    expect(decoderLog(1, 'pocsag1200').entries).toHaveLength(0);
  });
  it('replaces OnAir vessel snapshots and retains incremental weak signal batches', () => {
    for (const count of [1, 2]) {
      addDecoderResult({ provider: 'onair', mode: 'ais', kind: 'table', data: { type: 'vessels', vessels: [{ mmsi: count }] } }, '1:1', 1);
      addDecoderResult({ provider: 'onair', mode: 'ft4', kind: 'table', data: { type: 'decodes', decodes: [{ msg: `CQ ${count}` }] } }, '1:2', 1);
    }
    expect(decoderLog(1, 'ais').entries).toHaveLength(1);
    expect(decoderLog(1, 'ft4').entries).toHaveLength(2);
  });
  it('replaces aircraft snapshots, orders identity before telemetry, and clears departed aircraft', () => {
    const host = document.createElement('section');
    addDecoderResult({ provider: 'onair', mode: 'adsb', kind: 'table', data: {
      type: 'aircraft', aircraft: [{ age_s: 1, speed_kt: 210, lon: 21, lat: 55, alt_ft: 12000, callsign: 'FLIGHT1', icao: 'ABC123' }],
    } }, '1:1', 1);
    renderDecoderResults(host, 1, 'adsb', false);
    expect([...host.querySelectorAll('th')].slice(0, 6).map(cell => cell.textContent))
      .toEqual(['ICAO', 'callsign', 'alt ft', 'lat', 'lon', 'speed kt']);
    addDecoderResult({ provider: 'onair', mode: 'adsb', kind: 'table', data: {
      type: 'aircraft', aircraft: [{ icao: 'DEF456', callsign: 'FLIGHT2' }],
    } }, '1:2', 1);
    renderDecoderResults(host, 1, 'adsb', false);
    expect(decoderLog(1, 'adsb').entries).toHaveLength(1);
    expect(decoderLog(1, 'adsb').entries[0].source).toBe('1:2');
    expect(host.textContent).toContain('FLIGHT2');
    expect(host.textContent).not.toContain('FLIGHT1');
    addDecoderResult({ provider: 'onair', mode: 'adsb', kind: 'table', data: { type: 'aircraft', aircraft: [] } }, '1:2', 1);
    renderDecoderResults(host, 1, 'adsb', false);
    expect(host.querySelector('tbody')).toBeNull();
    expect(host.textContent).not.toContain('FLIGHT2');
  });
  it('replaces GPS tracking and PVT independently while retaining actual navigation messages', () => {
    const host = document.createElement('section');
    for (const prn of [7, 12]) {
      addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: {
        table: 'satellites', rows: [{ tracking: true, nav_word_valid: true, doppler_hz: -1234, cn0_db_hz: 42, channel: 0, prn }],
      } }, '1:1', 1);
      addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: {
        table: 'navigation', rows: [{ prn, tow_ms: 1000, nav_message: `bits ${prn}` }],
      } }, '1:1', 1);
    }
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: {
      table: 'pvt', rows: [{ valid: false, lat_deg: null, lon_deg: null, height_m: null }],
    } }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(decoderLog(1, 'gps').entries).toHaveLength(4);
    const tracking = [...host.querySelectorAll('article')].find(article => article.querySelector('strong')?.textContent?.startsWith('Satellite tracking'))!;
    expect([...tracking.querySelectorAll('th')].map(cell => cell.textContent))
      .toEqual(['PRN', 'channel', 'C/N0 (dB-Hz)', 'doppler Hz', 'tracking', 'nav word valid']);
    expect(tracking.querySelector('td')?.textContent).toBe('12');
    expect(host.textContent).toContain('No valid position fix');
    expect(host.textContent).toContain('bits 7');
    expect(host.textContent).toContain('bits 12');
  });
  it('hides stale PVT coordinates while preserving the original fields in Details', () => {
    const data = { table: 'pvt', rows: [{ valid: true, stale: true, lat_deg: 55.12345, lon_deg: 21.12345, height_m: 100 }] };
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data }, '1:1', 1);
    const host = document.createElement('section');
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: {
      table: 'pvt', rows: [{ valid: true, lat_deg: 56.54321, lon_deg: 22.12345, height_m: 200 }],
    } }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.querySelector('tbody')?.textContent).toContain('56.54321');
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.querySelector('tbody')?.textContent).toContain('No valid position fix');
    expect(host.querySelector('tbody')?.textContent).not.toContain('55.12345');
    expect(host.querySelector('tbody')?.textContent).not.toContain('56.54321');
    expect(decoderLog(1, 'gps').entries).toHaveLength(1);
    const details = host.querySelector('details')!;
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    expect(JSON.parse(details.querySelector('pre')!.textContent!)).toEqual(data);
  });
  it('marks bench GPS positions prominently and preserves provenance only in original Details', () => {
    const data = { table: 'pvt', bench: true, center_hz: 435_000_000, sourceFrequencyHz: 435_000_000,
      nominalCarrierHz: 1_575_420_000, input_samples: 4000000, input_rate_hz: 4000000, gnss_version: 'test-version',
      rows: [{ valid: true, lat_deg: 55.12345, lon_deg: 21.12345 }] };
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data }, '1:1', 1);
    const host = document.createElement('section');
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.querySelector('.decoder-origin')?.textContent).toBe('Bench signal · simulated position');
    expect(host.querySelector('tbody')?.textContent).toContain('55.12345');
    expect(host.textContent).not.toContain('435000000');
    expect(host.textContent).not.toContain('test-version');
    const details = host.querySelector('details')!;
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    expect(JSON.parse(details.querySelector('pre')!.textContent!)).toEqual(data);
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: { ...data, bench: false } }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.querySelector('.decoder-origin')).toBeNull();
  });
  it('clears GPS fixes on reset and stream changes without discarding navigation history', () => {
    const host = document.createElement('section');
    const fix = { provider: 'gnss-sdr', mode: 'gps', kind: 'table' as const, data: {
      table: 'pvt', rows: [{ valid: true, lat_deg: 55.12345, lon_deg: 21.12345, height_m: 100 }],
    } };
    addDecoderResult(fix, '1:1', 1);
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: { table: 'navigation', rows: [{ prn: 7, nav_message: 'real bits' }] } }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.textContent).toContain('Valid position fix');
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'status', data: { state: 'stopped', reason: 'reconfigured', fix: { valid: false } } }, '1:1', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.textContent).not.toContain('55.12345');
    expect(host.textContent).toContain('No valid position fix');
    expect(host.textContent).toContain('real bits');
    addDecoderResult(fix, '1:2', 1);
    addDecoderResult({ provider: 'gnss-sdr', mode: 'gps', kind: 'table', data: { table: 'satellites', rows: [{ prn: 12, tracking: true }] } }, '1:3', 1);
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.textContent).not.toContain('55.12345');
    expect(decoderLog(1, 'gps').entries.some(entry => entry.data !== null && typeof entry.data === 'object'
      && 'table' in entry.data && entry.data.table === 'pvt')).toBe(false);
    clearDecoderLog(1, 'gps');
    renderDecoderResults(host, 1, 'gps', false);
    expect(host.querySelector('article')).toBeNull();
  });
  it('retains readable RTTY text chunks with line breaks within the shared history bound', () => {
    for (let index = 0; index <= DECODER_LOG_ENTRIES; index++) {
      addDecoderResult({ provider: 'fldigi', mode: 'rtty', kind: 'text', data: { text: `CQ ${index}\nDE TEST` } }, '1:1', 1);
    }
    const host = document.createElement('section');
    renderDecoderResults(host, 1, 'rtty', false);
    expect(decoderLog(1, 'rtty').entries).toHaveLength(DECODER_LOG_ENTRIES);
    expect(host.querySelector('article > pre')?.textContent).toBe(`CQ ${DECODER_LOG_ENTRIES}\nDE TEST`);
  });
  it('renders real fields and paginates arrays without hiding text in JSON', () => {
    addDecoderResult({ provider: 'onair', mode: 'aprs', kind: 'table', data: {
      v: 1, kind: 'aprs', type: 'packets', sourceFrequencyHz: 144390000, onairFrequencyHz: 144390000,
      frequencyOffsetHz: 0, frames_ok: 400, dial_hz: 144390000,
      state: 'receiving', frames: Array.from({ length: 30 }, (_, index) => ({ from: `CALL${index}`, summary: `packet ${index}` })),
    } }, '1:1', 1);
    const host = document.createElement('section');
    document.body.appendChild(host);
    renderDecoderResults(host, 1, 'aprs', false);
    expect(host.querySelector('dl')?.textContent).toContain('receiving');
    expect(host.querySelector('dl')?.textContent).toContain('dial Hz');
    expect(host.querySelector('dl')?.textContent).not.toContain('source');
    expect(host.querySelector('dl')?.textContent).not.toContain('frames ok');
    expect(host.querySelector('dl')?.textContent).not.toContain('packets');
    expect(host.querySelector('tbody')?.textContent).toContain('packet 0');
    expect(host.querySelectorAll('tbody tr')).toHaveLength(25);
    const next = [...host.querySelectorAll('button')].find(button => button.textContent === '›')!;
    next.click();
    expect(host.querySelector('tbody')?.textContent).toContain('packet 29');
    expect(host.querySelectorAll('tbody tr')).toHaveLength(5);
    const row = host.querySelector('tbody tr');
    renderDecoderResults(host, 1, 'aprs', false);
    expect(host.querySelector('tbody tr')).toBe(row);
  });
  it('assembles PNG strips by source identity and retains the same picture across updates', () => {
    const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';
    const strip = { id: 7, label: 'Meteor', w: 1, h: 2, ch: 1, y: 0, rows: 1, pw: 1, ph: 1, png };
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'image', data: strip }, '1:1', 1);
    const id = decoderLog(1, 'meteor').entries[0].id;
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'image', data: { ...strip, y: 1, final: true } }, '1:1', 1);
    expect(decoderLog(1, 'meteor').entries).toHaveLength(1);
    expect(decoderLog(1, 'meteor').entries[0].id).toBe(id);
    const host = document.createElement('section');
    renderDecoderResults(host, 1, 'meteor', false);
    expect(host.querySelectorAll('img')).toHaveLength(2);
    expect(host.querySelectorAll('img')[1].style.top).toBe('50%');
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'image', data: strip }, '1:2', 1);
    expect(decoderLog(1, 'meteor').entries).toHaveLength(2);
    addDecoderResult({ provider: 'onair', mode: 'meteor', kind: 'status', data: { type: 'image_end', id: 7, rows: 2 } }, '1:2', 1);
    expect(decoderLog(1, 'meteor').entries).toHaveLength(2);
    expect(decoderLog(1, 'meteor').entries[0].data).toMatchObject({ final: true });
    renderDecoderResults(host, 1, 'meteor', false);
    expect(host.querySelector('.decoder-result strong')?.textContent).toContain('Complete');
  });
});
