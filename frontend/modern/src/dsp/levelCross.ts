// Level-trigger condition for the swept mode, evaluated in software.
//
// The RTA device trigger fires on a threshold crossing; a swept analyser produces one whole
// trace per sweep, so the same idea is applied across two consecutive sweeps: a bin that
// crosses the threshold between the previous and the current trace counts as one event.
export type TriggerEdge = 'rising' | 'falling' | 'double';

/**
 * Index of the first bin that crosses `threshold` between `prev` and `cur`, or -1.
 * Boundaries are inclusive; bins that are invalid in either trace are ignored.
 */
export function levelCrossing(
  prev: ArrayLike<number>,
  cur: ArrayLike<number>,
  threshold: number,
  edge: TriggerEdge,
  count?: number,
): number {
  if (!Number.isFinite(threshold)) return -1;
  const n = count === undefined
    ? Math.min(prev.length, cur.length)
    : Math.min(count, prev.length, cur.length);
  for (let i = 0; i < n; i++) {
    const a = prev[i];
    const b = cur[i];
    if (!Number.isFinite(a) || !Number.isFinite(b)) continue;
    const rising = a < threshold && b >= threshold;
    const falling = a > threshold && b <= threshold;
    if (edge === 'double' ? (rising || falling) : edge === 'falling' ? falling : rising) return i;
  }
  return -1;
}
