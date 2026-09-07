// Multi-marker peak tracking using the active trace's ranked signal peaks.
import * as S from '../core/store';
import { smoothForDisplay } from './smooth';

interface RankedPeak { idx: number; amp: number; freq: number; }

function activePowers(): Float32Array | null {
  if (S.rtaMode) {
    return S.rtaDisplays[S.activeTraceIdx] || (S.rtaData?.spec as Float32Array) || null;
  }
  const trace = S.traces[S.activeTraceIdx];
  if (!trace?.powers) return null;
  return S.smoothBins > 1
    ? smoothForDisplay(trace.powers, trace.mode)
    : trace.powers;
}

function peakThreshold(): number {
  const input = document.getElementById('input-peakthr') as HTMLInputElement | null;
  const value = input ? Number(input.value) : -200;
  return isFinite(value) ? value : -200;
}

export function rankedSignalPeaks(): RankedPeak[] {
  const powers = activePowers();
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
        freq: S.freqArray?.[i] ?? S.centerHz,
      });
    }
  }
  return peaks.sort((a, b) => b.amp - a.amp);
}

function assign(
  marker: S.MarkerState,
  peaks: RankedPeak[],
  occupied: number[],
  preferredIndex?: number,
  preferredFrequency?: number | null,
): boolean {
  const available = peaks.filter(candidate =>
    !occupied.some(index => Math.abs(index - candidate.idx) <= 5));
  if (!available.length) return false;
  let peak: RankedPeak | undefined;
  if (preferredFrequency != null && S.freqArray && S.freqArray.length > 1) {
    const nearest = available.reduce((best, candidate) =>
      Math.abs(candidate.freq - preferredFrequency) < Math.abs(best.freq - preferredFrequency)
        ? candidate : best);
    const span = Math.abs(S.freqArray[S.freqArray.length - 1] - S.freqArray[0]);
    if (Math.abs(nearest.freq - preferredFrequency) <= span * 0.2) peak = nearest;
  } else if (preferredIndex !== undefined) {
    const nearest = available.reduce((best, candidate) =>
      Math.abs(candidate.idx - preferredIndex) < Math.abs(best.idx - preferredIndex)
        ? candidate : best);
    const points = S.freqArray?.length || 1000;
    const trackingWindow = Math.max(20, Math.floor(points * 0.2));
    if (Math.abs(nearest.idx - preferredIndex) <= trackingWindow) peak = nearest;
  }
  peak ||= available[0];
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
    if (assign(marker, peaks, occupied, previous, marker.freq) && marker.idx !== previous) changed = true;
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
