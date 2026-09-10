import { describe, expect, it } from 'vitest';
import {
  buildLimitArray,
  evaluateAgainst,
  evaluateLimits,
  limitAt,
  normalizePoints,
  violationRuns,
} from '../dsp/limits';

const flat = [{ freqHz: 1e9, level: -30 }, { freqHz: 2e9, level: -30 }];
const ramp = [{ freqHz: 1e9, level: -30 }, { freqHz: 2e9, level: -10 }];

describe('limitAt', () => {
  it('interpolates linearly between points', () => {
    expect(limitAt(1.5e9, ramp)).toBeCloseTo(-20, 9);
    expect(limitAt(1.25e9, ramp)).toBeCloseTo(-25, 9);
  });

  it('extends the end values outside the defined range', () => {
    expect(limitAt(0.5e9, ramp)).toBe(-30);
    expect(limitAt(5e9, ramp)).toBe(-10);
  });

  it('sorts unsorted input before interpolating', () => {
    const unsorted = [{ freqHz: 2e9, level: -10 }, { freqHz: 1e9, level: -30 }];
    expect(limitAt(1.5e9, unsorted)).toBeCloseTo(-20, 9);
  });

  it('returns null when there are no usable points', () => {
    expect(limitAt(1e9, [])).toBeNull();
    expect(limitAt(1e9, [{ freqHz: NaN, level: -30 }])).toBeNull();
  });
});

describe('normalizePoints', () => {
  it('drops non-finite entries and sorts by frequency', () => {
    const out = normalizePoints([
      { freqHz: 3e9, level: -1 },
      { freqHz: Infinity, level: -2 },
      { freqHz: 1e9, level: -3 },
      { freqHz: 2e9, level: NaN },
    ]);
    expect(out).toEqual([{ freqHz: 1e9, level: -3 }, { freqHz: 3e9, level: -1 }]);
  });
});

describe('buildLimitArray', () => {
  it('produces one level per bin and NaN for invalid frequencies', () => {
    const arr = buildLimitArray([1e9, 1.5e9, NaN, 2e9], ramp)!;
    expect(arr[0]).toBeCloseTo(-30, 9);
    expect(arr[1]).toBeCloseTo(-20, 9);
    expect(Number.isNaN(arr[2])).toBe(true);
    expect(arr[3]).toBeCloseTo(-10, 9);
  });

  it('returns null without points', () => {
    expect(buildLimitArray([1e9, 2e9], [])).toBeNull();
  });
});

describe('evaluateAgainst / evaluateLimits', () => {
  const freqs = new Float64Array([1e9, 1.5e9, 2e9]);

  it('passes when every bin is at or below the limit', () => {
    const ev = evaluateLimits(freqs, new Float32Array([-31, -30, -40]), flat);
    expect(ev.pass).toBe(true);
    expect(ev.violations).toBe(0);
    expect(ev.worst).toBeNull();
    expect(ev.checked).toBe(3);
  });

  it('reports the violation count, worst margin and its frequency', () => {
    const ev = evaluateLimits(freqs, new Float32Array([-29, -25, -40]), flat);
    expect(ev.pass).toBe(false);
    expect(ev.violations).toBe(2);
    expect(ev.worst!.margin).toBeCloseTo(5, 6);
    expect(ev.worst!.freqHz).toBe(1.5e9);
    expect(ev.worst!.limit).toBe(-30);
  });

  it('honours the tolerance before counting a bin as exceeding', () => {
    const levels = new Float32Array([-29.5, -29.5, -40]);
    expect(evaluateLimits(freqs, levels, flat).violations).toBe(2);
    expect(evaluateLimits(freqs, levels, flat, 1).violations).toBe(0);
  });

  it('skips bins with invalid levels but still counts the rest', () => {
    const ev = evaluateAgainst(
      new Float32Array([NaN, -25, -40]),
      buildLimitArray(freqs, flat)!,
      freqs,
    );
    expect(ev.checked).toBe(2);
    expect(ev.violations).toBe(1);
  });

  it('passes vacuously with no points at all', () => {
    const ev = evaluateLimits(freqs, new Float32Array([0, 0, 0]), []);
    expect(ev).toEqual({ checked: 0, violations: 0, worst: null, pass: true });
  });

  it('judges a shaped limit only where it is actually exceeded', () => {
    // Ramp limit -30 -> -10 dBm. A flat -15 dBm signal only exceeds it near the low end:
    // at 1.75 GHz the limit is exactly -15 (equal, not a violation) and beyond that it is looser.
    const f2 = new Float64Array([1.0e9, 1.75e9, 1.9e9]);
    const ev = evaluateLimits(f2, new Float32Array([-15, -15, -15]), ramp);
    expect(ev.violations).toBe(1);
    expect(ev.checked).toBe(3);
    expect(ev.worst!.freqHz).toBe(1.0e9);
    expect(ev.worst!.margin).toBeCloseTo(15, 6);
  });
});

describe('violationRuns', () => {
  it('merges contiguous violating bins into inclusive runs', () => {
    const levels = new Float32Array([-40, -20, -20, -40, -20]);
    const lim = buildLimitArray([1e9, 1e9, 1e9, 1e9, 1e9], flat)!;
    expect(violationRuns(levels, lim)).toEqual([{ start: 1, end: 2 }, { start: 4, end: 4 }]);
  });

  it('returns nothing when the trace stays below the limit', () => {
    const levels = new Float32Array([-40, -50, -60]);
    const lim = buildLimitArray([1e9, 1e9, 1e9], flat)!;
    expect(violationRuns(levels, lim)).toEqual([]);
  });

  it('ignores invalid samples instead of ending a run early on NaN', () => {
    const levels = new Float32Array([-20, NaN, -20]);
    const lim = buildLimitArray([1e9, 1e9, 1e9], flat)!;
    expect(violationRuns(levels, lim)).toEqual([{ start: 0, end: 0 }, { start: 2, end: 2 }]);
  });
});
