// Software level trigger for the swept mode (SWP).
//
// master's RTA trigger is a device feature (level + edge + timing inside RTA_Profile). The
// swept engine has no level trigger at all, so the same user-facing behaviour is produced
// here: while armed the display keeps sweeping live, and the first sweep whose trace
// crosses the threshold (relative to the previous sweep) is frozen and marked TRIGGERED.
import * as S from '../core/store';
import { getDisplayPowers } from '../dsp/peaks';
import { levelCrossing } from '../dsp/levelCross';

type HitListener = (freqHz: number, level: number) => void;
const listeners = new Set<HitListener>();

export function onSwpHit(fn: HitListener): void {
  listeners.add(fn);
}

export function armSwpTrigger(): void {
  S.setSwpPrev(null);
  S.setSwpHold(false);
  S.setSwpArmed(true);
  S.setTrigArmed(true);
  S.setTrigWaiting(true);
  S.setTrigHit(false);
}

export function disarmSwpTrigger(): void {
  S.setSwpArmed(false);
  S.setSwpHold(false);
  S.setSwpPrev(null);
  S.setTrigArmed(false);
  S.setTrigWaiting(false);
  S.setTrigHit(false);
}

/** Called from the SWP frame path for every trace. */
export function evaluateSwpTrigger(): void {
  if (!S.swpArmed || S.swpHold) return;
  const d = getDisplayPowers();
  const f = S.freqArray;
  if (!d || !f || d.length < 2) return;
  const n = Math.min(d.length, f.length);
  const prev = S.swpPrev;
  if (prev && prev.length === n) {
    const idx = levelCrossing(prev, d, S.trigLevel, S.swpEdge, n);
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
  const copy = new Float32Array(n);
  copy.set(d.subarray(0, n));
  S.setSwpPrev(copy);
}
