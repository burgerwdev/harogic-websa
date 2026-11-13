import { describe, expect, it } from 'vitest';
import { advancePacketDensity, densityLut, probabilityColour, densityPalette, paintProbabilityDensity } from '../dsp/rtaDensity';
import { isLightTheme, setTheme, THEMES } from '../core/theme';

const freq = new Float64Array([100, 101]);
const spec = new Float32Array([-20, -20]);
const packet = (values: number[], sequence = 1n) => ({
  samples: new Uint8Array(values), scale: 0.5, offset: -120, sequence,
});

describe('acquired FFT probability density', () => {
  it('counts all spectra equally at every power, including noise and late packet events', () => {
    const grid = advancePacketDensity(null, packet([20, 20, 20, 80, 80, 80, 20, 80]), spec, freq, 256, 1, 1, 0)!;
    const probability = (column: number, value: number) => grid.values[column * 256 + 255 - value] / grid.population!;
    expect(grid.spectra).toBe(4);
    expect(probability(0, 20)).toBe(.75);
    expect(probability(0, 80)).toBe(.25);
    expect(probability(1, 20)).toBe(.25);
    expect(probability(1, 80)).toBe(.75);
  });

  it('ages hit counts by elapsed time and ignores replayed completed packets', () => {
    const grid = advancePacketDensity(null, packet([20, 20]), spec, freq, 256, 1, 1, 0)!;
    advancePacketDensity(grid, packet([80, 80], 2n), spec, freq, 256, 1, 1, 1000);
    expect(grid.values[235]).toBeCloseTo(.5);
    expect(grid.values[175]).toBe(1);
    expect(grid.population).toBe(1.5);
    const revision = grid.revision;
    advancePacketDensity(grid, packet([80, 80], 2n), spec, freq, 256, 1, 1, 2000);
    expect(grid.population).toBe(1.5);
    expect(grid.revision).toBe(revision);
    expect(grid.spectra).toBe(2);
  });

  it('keeps every hit when packets arrive together and resets on a new acquisition', () => {
    const first = advancePacketDensity(null, packet([20, 20]), spec, freq, 256, 1, 1, 0)!;
    advancePacketDensity(first, packet([80, 80], 2n), spec, freq, 256, 1, 1, 0);
    expect(first.population).toBe(2);
    const next = advancePacketDensity(first, packet([80, 80], 2n), spec, freq, 256, 1, 2, 0)!;
    expect(next).not.toBe(first);
    expect(next.population).toBe(1);
    expect(next.values[235]).toBe(0);
  });

  it('keeps calibrated dBm history when the SDK updates its amplitude offset', () => {
    const grid = advancePacketDensity(null, packet([20, 20]), spec, freq, 256, 1, 1, 0)!;
    const corrected = { ...packet([21, 21], 2n), offset: -120.5 };
    expect(advancePacketDensity(grid, corrected, spec, freq, 256, 1, 1, 0)).toBe(grid);
    expect(grid.values[235]).toBe(2);
    expect(grid.population).toBe(2);
    expect(grid.spectra).toBe(2);
  });

  it('preserves very rare events while bounding long gaps and clearing disabled history', () => {
    const grid = advancePacketDensity(null, packet([20, 20]), spec, freq, 256, 1, 1, 0)!;
    advancePacketDensity(grid, packet([80, 80], 2n), spec, freq, 256, 1, 1, 10000);
    expect(grid.values[235]).toBeCloseTo(1 / 1024);
    expect(advancePacketDensity(grid, packet([80, 80], 3n), spec, freq, 256, 0, 1, 10001)).toBeNull();
  });

  it.each(THEMES)('uses a distinct ordered probability palette for $id', theme => {
    setTheme(theme.id);
    expect(isLightTheme()).toBe(theme.scheme === 'light');
    const lut = densityLut(isLightTheme());
    const levels = [.0001, .001, .01, .1, 1].map(p => probabilityColour(p, 1));
    expect(new Set(levels.map(i => lut[i])).size).toBe(5);
    expect(levels).toEqual([...levels].sort((a, b) => a - b));
    expect(levels.at(-1)).toBe(255);
  });

  it('draws contiguous amplitude cells without dark seams at fractional pixel boundaries', () => {
    const pixels = new Uint8ClampedArray(20 * 4);
    paintProbabilityDensity(new Float32Array([4, 4, 4]), 1, 3, pixels,
      3.25, 6.5, 6.5, densityPalette(1, densityLut(false), true));
    const expected = Array.from(pixels.slice(4 * 4, 5 * 4));
    for (let y = 4; y < 16; y++) expect(Array.from(pixels.slice(y * 4, (y + 1) * 4))).toEqual(expected);
    expect(expected[3]).toBeGreaterThan(200);
  });
});
