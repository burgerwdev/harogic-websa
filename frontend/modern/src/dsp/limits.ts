// Limit lines: a piecewise-linear level limit over frequency plus pass/fail evaluation.
//
// The limit is defined by (frequency, level) points. Between points the level
// interpolates linearly; outside the defined frequency range the end values are
// extended, so a two-point limit behaves as the usual flat limit across the span.
// Pure logic only (no DOM, no canvas) so it can be unit tested directly.

export interface LimitPoint { freqHz: number; level: number; }
export interface LimitViolation { freqHz: number; level: number; limit: number; margin: number; }
export interface LimitEval { checked: number; violations: number; worst: LimitViolation | null; pass: boolean; }
export interface ViolationRun { start: number; end: number; }

/** Drop non-finite points and sort by frequency (a limit is only meaningful sorted). */
export function normalizePoints(points: readonly LimitPoint[]): LimitPoint[] {
  return points
    .filter((p) => Number.isFinite(p.freqHz) && Number.isFinite(p.level))
    .map((p) => ({ freqHz: p.freqHz, level: p.level }))
    .sort((a, b) => a.freqHz - b.freqHz);
}

function limitFromSorted(freqHz: number, pts: readonly LimitPoint[]): number {
  const first = pts[0];
  const last = pts[pts.length - 1];
  if (freqHz <= first.freqHz) return first.level;
  if (freqHz >= last.freqHz) return last.level;
  for (let i = 1; i < pts.length; i++) {
    const b = pts[i];
    if (freqHz <= b.freqHz) {
      const a = pts[i - 1];
      const d = b.freqHz - a.freqHz;
      if (d <= 0) return b.level;
      return a.level + (b.level - a.level) * ((freqHz - a.freqHz) / d);
    }
  }
  return last.level;
}

/** Limit level at one frequency (convenience wrapper; sorts on every call). */
export function limitAt(freqHz: number, points: readonly LimitPoint[]): number | null {
  const pts = normalizePoints(points);
  if (!pts.length) return null;
  return limitFromSorted(freqHz, pts);
}

/**
 * Per-bin limit levels, computed once per frame: sorting happens once instead of
 * once per bin, which matters at 4000 points and 25 fps.
 */
export function buildLimitArray(
  freqs: ArrayLike<number>,
  points: readonly LimitPoint[],
  count?: number,
): Float64Array | null {
  const pts = normalizePoints(points);
  if (!pts.length) return null;
  const n = count === undefined ? freqs.length : Math.min(count, freqs.length);
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    const f = freqs[i];
    out[i] = Number.isFinite(f) ? limitFromSorted(f, pts) : NaN;
  }
  return out;
}

/** Compare a trace against precomputed limit levels. A bin counts as exceeding when margin > tolDb. */
export function evaluateAgainst(
  levels: ArrayLike<number>,
  limitArr: ArrayLike<number>,
  freqs: ArrayLike<number>,
  tolDb = 0,
): LimitEval {
  const n = Math.min(levels.length, limitArr.length);
  let checked = 0;
  let violations = 0;
  let worst: LimitViolation | null = null;
  for (let i = 0; i < n; i++) {
    const v = levels[i];
    const lim = limitArr[i];
    if (!Number.isFinite(v) || !Number.isFinite(lim)) continue;
    checked++;
    const margin = v - lim;
    if (margin > tolDb) {
      violations++;
      if (!worst || margin > worst.margin) worst = { freqHz: freqs[i], level: v, limit: lim, margin };
    }
  }
  return { checked, violations, worst, pass: violations === 0 };
}

/** Convenience wrapper for callers that have points instead of a precomputed array. */
export function evaluateLimits(
  freqs: ArrayLike<number>,
  levels: ArrayLike<number>,
  points: readonly LimitPoint[],
  tolDb = 0,
): LimitEval {
  const n = Math.min(freqs.length, levels.length);
  const arr = buildLimitArray(freqs, points, n);
  if (!arr) return { checked: 0, violations: 0, worst: null, pass: true };
  return evaluateAgainst(levels, arr, freqs, tolDb);
}

/** Contiguous index runs (inclusive) above the limit, for a cheap red overlay. */
export function violationRuns(
  levels: ArrayLike<number>,
  limitArr: ArrayLike<number>,
  tolDb = 0,
): ViolationRun[] {
  const n = Math.min(levels.length, limitArr.length);
  const runs: ViolationRun[] = [];
  let start = -1;
  for (let i = 0; i < n; i++) {
    const v = levels[i];
    const lim = limitArr[i];
    const bad = Number.isFinite(v) && Number.isFinite(lim) && v - lim > tolDb;
    if (bad) {
      if (start < 0) start = i;
    } else if (start >= 0) {
      runs.push({ start, end: i - 1 });
      start = -1;
    }
  }
  if (start >= 0) runs.push({ start, end: n - 1 });
  return runs;
}
