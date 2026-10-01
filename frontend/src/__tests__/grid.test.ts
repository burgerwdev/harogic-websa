import { describe, expect, it } from 'vitest';

import { alignToDisplayWindow } from '../dsp/grid';

function grid(lo: number, hi: number, n: number): Float64Array {
  return Float64Array.from({ length: n }, (_, i) => lo + (i * (hi - lo)) / (n - 1));
}

describe('alignToDisplayWindow', () => {
  it('leaves a frame alone when the capture grid already spans the display window', () => {
    const freq = grid(100, 108, 9);
    const spec = new Float32Array(Array.from({ length: 9 }, (_, i) => -100 + i));
    const out = alignToDisplayWindow(freq, spec, 100, 108);
    expect(out.shifted).toBe(false);
    expect(out.freq).toBe(freq);
    expect(out.spec).toBe(spec);
  });

  it('rebins an offset capture to the display window and leaves the uncovered edge empty', () => {
    // Display window 2..10, capture grid 4..12: a 2-bin hardware offset.
    const capFreq = grid(4, 12, 9);
    const capSpec = Float32Array.from({ length: 9 }, (_, i) => i);
    const out = alignToDisplayWindow(capFreq, capSpec, 2, 10);
    expect(out.shifted).toBe(true);
    expect(out.freq[0]).toBeCloseTo(2, 9);
    expect(out.freq[8]).toBeCloseTo(10, 9);
    // The first 2 bins are outside the capture range: a real gap, not interpolated data.
    expect(Number.isNaN(out.spec[0])).toBe(true);
    expect(Number.isNaN(out.spec[1])).toBe(true);
    // Inside the capture the values follow the capture bins (display f -> capture index f-4).
    expect(Array.from(out.spec.slice(2))).toEqual([0, 1, 2, 3, 4, 5, 6]);
    // The user's centre lands at the centre of the frequency axis.
    expect((out.freq[0] + out.freq[8]) / 2).toBeCloseTo(6, 9);
  });

  it('interpolates fractionally offset grids instead of snapping', () => {
    const capFreq = grid(0, 8, 9);           // step 1
    const capSpec = Float32Array.from({ length: 9 }, (_, i) => i * 10);
    const out = alignToDisplayWindow(capFreq, capSpec, 1.5, 9.5);   // 1.5-bin shift
    expect(out.shifted).toBe(true);
    expect(out.spec[0]).toBeCloseTo(15, 5);  // halfway between 10 and 20
    expect(Number.isNaN(out.spec[8])).toBe(true);   // 9.5 is past the capture
  });
});
