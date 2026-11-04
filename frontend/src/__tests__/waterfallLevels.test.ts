import { describe, expect, it } from 'vitest';
import { chooseViewFloor, observedFloor, WaterfallAutoLevels } from '../dsp/waterfallLevels';
import { waterfallLUT } from '../render/waterfallPalette';

describe('SDR waterfall levels', () => {
  it('uses finite native bins and falls back for narrow, missing or signal-filled views', () => {
    const levels = new Float32Array(1000).fill(-90);
    levels.fill(-120, 100, 200); levels[150] = -10; levels[160] = NaN;
    const local = observedFloor(100, i => levels[100 + i]);
    expect(local.count).toBe(99);
    expect(chooseViewFloor(local, 100, -90)).toBeCloseTo(-120, 0);
    expect(chooseViewFloor({ floor: -20, count: 100 }, 100, -90)).toBe(-90);
    expect(chooseViewFloor({ floor: -120, count: 31 }, 100, -90)).toBe(-90);
    expect(chooseViewFloor(local, 1000, -90)).toBe(-90);
    expect(observedFloor(10, () => NaN).floor).toBeNaN();
  });
  it('keeps a weak station at the same colour when a much stronger carrier appears', () => {
    const tracker = new WaterfallAutoLevels();
    const bins = new Float32Array(4096).fill(-110);
    bins[100] = -102;
    const before = tracker.update(bins, 0);
    bins[200] = -15;
    const after = tracker.update(bins, 33);
    expect(after).toEqual(before);
    expect(after.hi - after.lo).toBe(55);
    expect(after.lo).toBeLessThan(-110); // Noise grain remains visible.
    expect((-102 - after.lo) / (after.hi - after.lo)).toBeGreaterThan(0.2);
  });

  it('smooths noise changes using elapsed time and reacquires on a capture reset', () => {
    const tracker = new WaterfallAutoLevels();
    const before = tracker.update(new Float32Array(128).fill(-110), 0);
    const changed = new Float32Array(128).fill(-100);
    const first = tracker.update(changed, 33);
    expect(first.lo).toBeGreaterThan(before.lo);
    expect(first.lo - before.lo).toBeLessThan(0.5);
    let settled = first;
    for (let i = 2; i <= 300; i++) settled = tracker.update(changed, i * 33);
    expect(settled.lo - before.lo).toBeCloseTo(10, 0);
    expect(tracker.update(new Float32Array(128).fill(-140), 10_000, true).lo).toBeLessThan(-145);
  });

  it('ignores nonfinite observations and keeps empty frames from changing the range', () => {
    const tracker = new WaterfallAutoLevels();
    const before = tracker.update([-110, -110, -100, NaN, Infinity, -Infinity], 0);
    expect(tracker.update([NaN, Infinity, -Infinity], 33)).toEqual(before);
    const empty = new WaterfallAutoLevels().update([], 0);
    expect(Number.isFinite(empty.lo) && Number.isFinite(empty.hi)).toBe(true);
  });
});

describe('waterfall palettes', () => {
  it.each(['classic', 'blue', 'viridis', 'amber'] as const)('%s distinguishes noise, weak and strong signals', palette => {
    const lut = waterfallLUT(palette, 1);
    const luminance = (pixel: number) => {
      const linear = (channel: number) => Math.pow(channel / 255, 2.2);
      return 0.2126 * linear(pixel & 255) + 0.7152 * linear((pixel >>> 8) & 255) + 0.0722 * linear((pixel >>> 16) & 255);
    };
    if (palette === 'classic') {
      const blue = lut[Math.round(lut.length * 0.24)];
      const green = lut[Math.round(lut.length * 0.52)];
      const hot = lut[Math.round(lut.length * 0.9)];
      expect((blue >>> 16) & 255).toBeGreaterThan(blue & 255);
      expect((green >>> 8) & 255).toBeGreaterThan((green >>> 16) & 255);
      expect(hot & 255).toBeGreaterThan((hot >>> 8) & 255);
    } else {
      // Compare signal steps rather than byte rounding between adjacent LUT entries.
      for (let i = 32; i < lut.length; i += 32) expect(luminance(lut[i])).toBeGreaterThan(luminance(lut[i - 32]));
    }
    expect(luminance(lut[0])).toBeLessThan(0.01);
    expect(luminance(lut[lut.length - 1])).toBeGreaterThan(0.75);
    expect(new Set(lut).size).toBeGreaterThan(256);
  });
});
