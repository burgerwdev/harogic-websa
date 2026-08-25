// Marker common: index/frequency/relocation (decoupled from render to avoid circular deps)
import * as S from './store';

export function markerFreqHz(idx: number): number {
  if (S.freqArray && S.freqArray.length > 1) return S.freqArray[Math.min(idx, S.freqArray.length - 1)];
  return S.centerHz;
}

// After the frequency axis updates, map each enabled marker to the nearest point of the new axis by its recorded frequency
export function retrackMarkers() {
  if (!S.freqArray) return;
  S.markers.forEach(m => {
    if (m.enabled && m.freq != null) {
      let best = 0, bd = Infinity;
      for (let i = 0; i < S.freqArray!.length; i++) {
        const d = Math.abs(S.freqArray![i] - m.freq);
        if (d < bd) { bd = d; best = i; }
      }
      m.idx = best;
    }
  });
}
