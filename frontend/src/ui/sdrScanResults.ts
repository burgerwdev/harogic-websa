import type { ScanSignal } from './sdrScan';
export type ScanSort = 'frequency' | 'width' | 'power' | 'activity' | 'type';
export interface ScanFilters { query: string; type: string; status: string; source: string; minPower: string; sort: ScanSort; descending: boolean }
export const defaultScanFilters = (): ScanFilters => ({ query: '', type: 'all', status: 'all', source: 'all', minPower: '', sort: 'frequency', descending: false });
export function scanSignalFamily(s: ScanSignal): string {
  if (s.protocol === 'DRM' || s.protocol === 'FT8') return 'digital';
  if (s.label === 'AM-like') return 'am';
  if (s.label === 'FM-like' || s.label === 'FM stereo pilot' || s.protocol === 'RDS') return 'fm';
  if (/FSK|PSK/.test(s.label) || s.protocol === 'DRM' || s.protocol === 'FT8') return 'digital';
  return s.label === 'carrier' ? 'carrier' : 'unknown';
}
export function scanResults(signals: readonly ScanSignal[], filter: ScanFilters, saved: (s: ScanSignal) => boolean = () => false, describe: (s: ScanSignal) => string = () => ''): ScanSignal[] {
  const terms = filter.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const rows = signals.filter(s => {
    if (s.activity === 'ignored') return false;
    if (filter.type !== 'all' && scanSignalFamily(s) !== filter.type) return false;
    if (filter.source !== 'all' && s.source !== filter.source) return false;
    if (filter.minPower.trim() && Number.isFinite(Number(filter.minPower)) && s.excess_db < Number(filter.minPower)) return false;
    if (filter.status === 'active' && s.activity !== 'active') return false;
    if (filter.status === 'complete' && (s.identify_state !== 'complete' || s.activity === 'stale')) return false;
    if (filter.status === 'pending' && !['idle', 'detecting', 'queued', 'waiting', 'sampling', 'analyzing'].includes(s.identify_state)) return false;
    if (filter.status === 'stale' && !['stale', 'unseen', 'snapshot', 'not_detected'].includes(s.activity)) return false;
    if (filter.status === 'saved' && !saved(s)) return false;
    const haystack = [(s.center_hz / 1e6).toFixed(6), (s.center_hz / 1e3).toFixed(3), s.label, s.protocol, s.identify_state, s.activity, describe(s)].join(' ').toLowerCase();
    return terms.every(term => haystack.includes(term));
  });
  const direction = filter.descending ? -1 : 1;
  return rows.sort((a, b) => {
    const value = filter.sort === 'frequency' ? a.center_hz - b.center_hz : filter.sort === 'width' ? a.bandwidth_hz - b.bandwidth_hz
      : filter.sort === 'power' ? a.excess_db - b.excess_db : filter.sort === 'activity' ? a.observed_hit_ratio - b.observed_hit_ratio : a.label.localeCompare(b.label);
    return value * direction || a.center_hz - b.center_hz || a.id.localeCompare(b.id);
  });
}
