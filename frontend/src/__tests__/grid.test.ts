import { describe, expect, it } from 'vitest';
import { alignToDisplayWindow, projectSpectrum, sameFrequencyGrid, visibleBinRange } from '../dsp/grid';

describe('native frequency geometry', () => {
  it('keeps close peaks, their valley, native positions and gaps through an offset display window', () => {
    const freq = Float64Array.from([100, 101.25, 102.5, 103.75, 105]);
    const spec = Float32Array.from([-90, -25, -95, -30, NaN]);
    const out = alignToDisplayWindow(freq, spec, 99.5, 106);
    expect(out.freq).toBe(freq);
    expect(out.spec).toBe(spec);
    expect([...out.spec.slice(1, 4)]).toEqual([-25, -95, -30]);
    expect(Number.isNaN(out.spec[4])).toBe(true);
    expect([...out.freq]).toEqual([100, 101.25, 102.5, 103.75, 105]);
  });

  it('selects only the visible bins and boundary neighbours, including exact boundaries', () => {
    const freq = Float64Array.from([0, 10, 20, 30, 40, 50, 60]);
    expect(visibleBinRange(freq, 20, 40)).toEqual([1, 6]);
    expect(visibleBinRange(freq, 21, 39)).toEqual([2, 5]);
    expect(visibleBinRange(freq, -10, 0)).toEqual([0, 2]);
    expect(visibleBinRange(freq, 60, 70)).toEqual([5, 7]);
  });

  it('detects shifted interior bins even when count and endpoints match', () => {
    expect(sameFrequencyGrid(Float64Array.from([0, 1, 3]), Float64Array.from([0, 2, 3]))).toBe(false);
    expect(sameFrequencyGrid(Float64Array.from([0, 1, 3]), Float64Array.from([0, 1, 3]))).toBe(true);
  });

  it('projects coarse columns by Hz and keeps empty coverage and the final carrier', () => {
    const freq = Float64Array.from([12, 13, 14, 18]);
    const spec = Float32Array.from([-60, -20, NaN, -25]);
    const out = projectSpectrum(freq, spec, 10, 18, new Float32Array(4));
    expect(Number.isNaN(out[0])).toBe(true);
    expect(out[1]).toBe(-20);
    expect(Number.isNaN(out[2])).toBe(true);
    expect(out[3]).toBe(-25);
  });
});
