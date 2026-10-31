// Multi-marker peak tracking using the active trace's ranked signal peaks.
import * as S from '../core/store';
import { centerHz } from '../ui/freqState';
import { getDisplayPowers } from './peaks';
import { peakThr } from '../ui/measurePrefs';

interface RankedPeak { idx: number; amp: number; freq: number; }

function peakThreshold(): number {
  const value = peakThr.get();
  return isFinite(value) ? value : -80;
}

export function rankedSignalPeaks(): RankedPeak[] {
  const powers = getDisplayPowers();
  if (!powers || powers.length < 3) return [];
  const threshold = peakThreshold();
  const peaks: RankedPeak[] = [];
  for (let i = 1; i < powers.length - 1; i++) {
    const value = powers[i];
    if (!isFinite(value) || value <= threshold) continue;
    if (value >= powers[i - 1] && value > powers[i + 1]) {
      peaks.push({
        idx: i,
        amp: value,
        freq: S.freqArray?.[i] ?? centerHz.get(),
      });
    }
  }
  return peaks.sort((a, b) => b.amp - a.amp);
}

function assign(
  marker: S.MarkerState,
  peaks: RankedPeak[],
  occupied: number[],
): boolean {
  const available = peaks.filter(candidate =>
    !occupied.some(index => Math.abs(index - candidate.idx) <= 5));
  if (!available.length) return false;
  const peak = available[0];
  marker.idx = peak.idx;
  marker.freq = peak.freq;
  occupied.push(peak.idx);
  return true;
}

export function assignMarkerToBestPeak(marker: S.MarkerState): boolean {
  const occupied = S.markers
    .filter(other => other.id !== marker.id && other.enabled && other.mode !== 'OFF')
    .map(other => other.idx);
  return assign(marker, rankedSignalPeaks(), occupied);
}

export function updateTrackingMarkers(): boolean {
  const tracked = S.markers
    .filter(marker => marker.tracking && marker.enabled && marker.mode !== 'OFF')
    .sort((a, b) => a.id - b.id);
  if (!tracked.length) return false;
  const peaks = rankedSignalPeaks();
  if (!peaks.length) return false;
  const occupied = S.markers
    .filter(marker => !marker.tracking && marker.enabled && marker.mode !== 'OFF')
    .map(marker => marker.idx);
  let changed = false;
  for (const marker of tracked) {
    const previous = marker.idx;
    if (assign(marker, peaks, occupied) && marker.idx !== previous) changed = true;
  }
  return changed;
}

export function toggleMarkerTracking(marker: S.MarkerState): void {
  marker.tracking = !marker.tracking;
  if (marker.tracking) {
    marker.enabled = true;
    if (marker.mode === 'OFF') marker.mode = 'NORMAL';
    updateTrackingMarkers();
  }
}
