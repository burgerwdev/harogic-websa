// Software level trigger for the swept mode (SWP).
//
// master's RTA trigger is a device feature (level + edge + timing inside RTA_Profile). The
// swept engine has no level trigger at all, so the same user-facing behaviour is produced
// here: while armed the display keeps sweeping live, and the first sweep whose trace
// crosses the threshold (relative to the previous sweep) is frozen and marked TRIGGERED.
import * as S from '../core/store';
import { trigEdge, trigLevel, triggerMode } from './triggerState';
import { levelCrossing } from '../dsp/levelCross';
import { sameFrequencyGrid } from '../dsp/grid';

type HitListener = (freqHz: number, level: number) => void;
const listeners = new Set<HitListener>();
let previousGrid: Float64Array | null = null;

export function onSwpHit(fn: HitListener): void {
  listeners.add(fn);
}

export function armSwpTrigger(): void {
  previousGrid = null;
  S.setSwpPrev(null);
  S.setSwpHold(false);
  S.setSwpArmed(true);
  S.setTrigArmed(true);
  S.setTrigWaiting(true);
  S.setTrigHit(false);
}


/** Called from the SWP frame path for every trace. */
export function evaluateSwpTrigger(d: Float32Array): void {
  if (triggerMode() !== 'std' || !S.swpArmed || S.swpHold) return;
  const f = S.freqArray;
  if (!f || d.length < 2) return;
  const n = Math.min(d.length, f.length);
  if (!sameFrequencyGrid(previousGrid, f)) S.setSwpPrev(null);
  previousGrid = f;
  const prev = S.swpPrev;
  if (prev && prev.length === n) {
    const idx = levelCrossing(prev, d, trigLevel.get(), trigEdge.get(), n);
    if (idx >= 0) {
      S.setSwpArmed(false);
      S.setSwpHold(true);                 // freeze the swept display
      S.setTrigArmed(false);
      S.setTrigWaiting(false);
      S.setTrigHit(true);
      listeners.forEach((fn) => fn(f[idx], d[idx]));
      return;
    }
  }
  const copy = prev?.length === n ? prev : new Float32Array(n);
  copy.set(d.subarray(0, n));
  S.setSwpPrev(copy);
}
