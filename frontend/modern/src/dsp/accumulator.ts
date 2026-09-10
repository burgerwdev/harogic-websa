// Shared trace accumulation used by both SWP and RTA.
//
// A single implementation keeps mode semantics identical in both acquisition paths:
//   CLEAR_WRITE / MAX_HOLD / MIN_HOLD / AVERAGE (finite N or ∞) / VIEW / OFF
// OFF is handled by the caller (data is retained); Clear uses resetTraceAccum().
import type { TraceState } from '../core/store';

/** Peak-preserving resample (local copy so this module has no cyclic import). */
function resamplePeak(src: Float32Array, newLen: number, isMaxHold: boolean): Float32Array {
  const out = new Float32Array(newLen);
  const oldLen = src.length;
  if (oldLen < 2 || newLen < 2) return new Float32Array(src.slice(0, newLen));
  for (let i = 0; i < newLen; i++) {
    const start = i * (oldLen - 1) / (newLen - 1);
    const end = (i + 1) * (oldLen - 1) / (newLen - 1);
    const j0 = Math.floor(start);
    const j1 = Math.min(oldLen - 1, Math.max(j0, Math.ceil(end)));
    let v = src[j0];
    for (let j = j0 + 1; j <= j1; j++) v = isMaxHold ? Math.max(v, src[j]) : Math.min(v, src[j]);
    out[i] = v;
  }
  return out;
}

/** Selectable average counts. 0 means continuous (unbounded) averaging. */
export const AVG_COUNTS = [2, 4, 8, 16, 32, 64, 128, 256, 0] as const;
export const AVG_DEFAULT = 16;

export function applyMode(t: TraceState, mode: string): void {
  t.mode = mode;
  if (mode !== 'VIEW' && mode !== t.prevMode) t.prevMode = mode;
  t.done = false;
  if (mode === 'AVERAGE') {
    if (t.powers && t.powers.length) {
      t.avgSum = new Float32Array(t.powers);   // seed from the trace on screen
      t.avgCount = 1;
    } else {
      t.avgSum = null;
      t.avgCount = 0;
    }
  } else {
    t.avgSum = null;
    t.avgCount = 0;
  }
}

/** Set the average target and restart averaging (changing N restarts the average). */
export function setAverageCount(t: TraceState, count: number): void {
  t.avgTarget = AVG_COUNTS.includes(count as (typeof AVG_COUNTS)[number]) ? count : AVG_DEFAULT;
  t.avgSum = null;
  t.avgCount = 0;
  t.done = false;
}

/**
 * Accumulate one frame into the trace. `data` is the frame after normalization.
 * Returns true when the trace changed.
 */
export function accumulateTrace(t: TraceState, data: Float32Array): boolean {
  if (t.mode === 'OFF') return false;

  if (t.mode === 'VIEW') {
    if (!t.powers || t.powers.length !== data.length) {
      t.powers = new Float32Array(data);
      return true;
    }
    return false;
  }

  if (!t.powers || t.powers.length !== data.length) {
    if (t.powers && (t.mode === 'MAX_HOLD' || t.mode === 'MIN_HOLD')
      && t.powers.length > 1 && data.length > 1) {
      t.powers = resamplePeak(t.powers, data.length, t.mode === 'MAX_HOLD');
    } else {
      t.avgSum = null;
      t.avgCount = 0;
      t.done = false;
      t.powers = new Float32Array(data);
      if (t.mode === 'AVERAGE') {
        t.avgSum = new Float32Array(data);
        t.avgCount = 1;
      }
    }
    return true;
  }

  if (t.mode === 'CLEAR_WRITE') {
    t.powers.set(data);
    return true;
  }
  if (t.mode === 'MAX_HOLD') {
    for (let i = 0; i < data.length; i++) if (data[i] > t.powers[i]) t.powers[i] = data[i];
    return true;
  }
  if (t.mode === 'MIN_HOLD') {
    for (let i = 0; i < data.length; i++) if (data[i] < t.powers[i]) t.powers[i] = data[i];
    return true;
  }
  if (t.mode === 'AVERAGE') {
    // Finite N: exponential moving average with alpha = 2/(N+1). It never stops, so the
    // trace keeps refreshing; N sets the smoothing depth. 0 (infinity) = cumulative mean
    // since the last reset.
    const target = t.avgTarget || 0;
    if (!t.avgSum || t.avgSum.length !== data.length) {
      t.avgSum = new Float32Array(data);
      t.avgCount = 1;
      if (target > 0) {
        t.powers.set(data);
        return true;
      }
    }
    if (target > 0) {
      const alpha = 2 / (target + 1);
      for (let i = 0; i < data.length; i++) t.powers[i] += (data[i] - t.powers[i]) * alpha;
      t.avgCount++;
      return true;
    }
    t.avgCount++;
    const inv = 1 / t.avgCount;
    for (let i = 0; i < data.length; i++) {
      t.avgSum[i] += data[i];
      t.powers[i] = t.avgSum[i] * inv;
    }
    return true;
  }
  return false;
}
