import { describe, expect, it } from 'vitest';
import { defaultScanFilters, scanResults } from '../ui/sdrScanResults';
import type { ScanSignal } from '../ui/sdrScan';
const row = (id: string, hz: number, label: string, excess: number, extra = {}): ScanSignal => ({ id, center_hz: hz, label, excess_db: excess, bandwidth_hz: 12000, source: 'scan', activity: 'active', identify_state: 'complete', observed_hit_ratio: 1, protocol: null, ...extra } as ScanSignal);
const rows = [row('a', 9.4e6, 'AM-like', 15), row('b', 101.7e6, 'FM stereo pilot', 30, { bandwidth_hz: 180000 }), row('c', 9.8e6, 'unknown', 6, { identify_state: 'waiting' }), row('d', 9.5e6, 'AM-like', 15, { activity: 'stale' })];
describe('scan result search and sorting', () => {
  it('searches MHz, kHz and localized descriptions without changing RF data', () => {
    expect(scanResults(rows, { ...defaultScanFilters(), query: '9.4' }).map(r => r.id)).toEqual(['a']);
    expect(scanResults(rows, { ...defaultScanFilters(), query: '9400' }).map(r => r.id)).toEqual(['a']);
    expect(scanResults(rows, { ...defaultScanFilters(), query: '广播' }, undefined, r => r.id === 'b' ? '广播 FM' : '').map(r => r.id)).toEqual(['b']);
  });
  it('combines type/status filters and keeps pending unknown signals findable', () => {
    expect(scanResults(rows, { ...defaultScanFilters(), type: 'am', status: 'complete' }).map(r => r.id)).toEqual(['a']);
    expect(scanResults(rows, { ...defaultScanFilters(), type: 'unknown', status: 'pending' }).map(r => r.id)).toEqual(['c']);
  });
  it('sorts numeric columns, keeps ties stable and does not mutate the source', () => {
    const order = rows.map(r => r.id); expect(scanResults(rows, { ...defaultScanFilters(), sort: 'power', descending: true }).map(r => r.id)).toEqual(['b', 'a', 'd', 'c']);
    expect(scanResults(rows, { ...defaultScanFilters(), sort: 'frequency', descending: true }).map(r => r.id)).toEqual(['b', 'c', 'd', 'a']); expect(rows.map(r => r.id)).toEqual(order);
  });
  it('filters added channels and minimum power, and omits ignored results', () => {
    expect(scanResults(rows, { ...defaultScanFilters(), status: 'saved' }, r => r.id === 'b').map(r => r.id)).toEqual(['b']);
    expect(scanResults(rows, { ...defaultScanFilters(), minPower: '20' }).map(r => r.id)).toEqual(['b']);
    expect(scanResults([row('i', 1e6, 'unknown', 1, { activity: 'ignored' })], defaultScanFilters())).toEqual([]);
  });
});
