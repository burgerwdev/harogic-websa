import { beforeEach, describe, expect, it } from 'vitest';
import { pushSpectrumRow, renderWaterfall, sliceDensityRow, sliceWaterfallRow } from '../render/waterfall';
import * as S from '../core/store';
import { wfRangeMode, wfLoDbm, wfHiDbm } from '../ui/waterfallState';
import { __testSeedCapture } from '../ui/spectrumViewport';

beforeEach(() => {
  S.resetWaterfall();
  wfRangeMode.set('fixed');
  wfLoDbm.set(-100);
  wfHiDbm.set(0);
});

describe('native waterfall history', () => {
  it('retains a close pair, valley, missing bin and final carrier on their native Hz', () => {
    const freq = Float64Array.from([10, 12, 14, 16, 18]);
    pushSpectrumRow(Float32Array.from([-20, -95, -30, NaN, -25]), freq, 8, 20, 100);
    const row = S.waterfallRows[0];
    expect([...row]).toEqual([80, 5, 70, 0, 75]);
    expect([...sliceWaterfallRow(row, 3, 10, 16)]).toEqual([80, 5, 70]);
    expect([...sliceWaterfallRow(row, 4, 15, 19)]).toEqual([0, 0, 75, 75]);
    expect([...sliceWaterfallRow(row, 3, 8, 11)]).toEqual([0, 0, 80]);
  });

  it('bounds oversized history while retaining a carrier in the last native bin', () => {
    const freq = Float64Array.from({ length: 32769 }, (_, i) => i);
    const levels = new Float32Array(freq.length).fill(-100);
    levels[levels.length - 1] = -10;
    for (let i = 0; i < 513; i++) pushSpectrumRow(levels, freq, 0, 32768, 100);
    expect(S.waterfallRows).toHaveLength(512);
    expect(S.waterfallRows[511]).toHaveLength(16384);
    expect(sliceWaterfallRow(S.waterfallRows[511], 1, 32767, 32768)[0]).toBe(90);
  });

  it('scrolls new rows without changing historical pixels and rebuilds correctly after reset', () => {
    const freq = Float64Array.from([10, 12, 14, 16]);
    let pixels = new Uint8ClampedArray();
    const context = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: (image: { data: Uint8ClampedArray }) => { pixels = image.data.slice(); },
    };
    const canvas = { width: 4, height: 3, getContext: () => context } as unknown as HTMLCanvasElement;
    __testSeedCapture({ lo: 10, hi: 16 });
    try {
      pushSpectrumRow(Float32Array.from([-80, -60, -40, -20]), freq, 10, 16, 100);
      renderWaterfall(canvas, 100);
      const originalTop = pixels.slice(0, 16);
      pushSpectrumRow(Float32Array.from([-20, -40, -60, -80]), freq, 10, 16, 100);
      renderWaterfall(canvas, 100);
      expect([...pixels.slice(16, 32)]).toEqual([...originalTop]);
      const incremental = pixels.slice();
      const rows = [...S.waterfallRows];
      S.resetWaterfall();
      // A reset can replace rows without changing the final push counter.
      for (const row of rows) S.pushWaterfallRow(row);
      canvas.width = 5;
      renderWaterfall(canvas, 100);
      canvas.width = 4;
      renderWaterfall(canvas, 100);
      expect([...pixels]).toEqual([...incremental]);
    } finally {
      __testSeedCapture(null);
    }
  });
});

describe('sliceDensityRow', () => {
	it('is identity when the row width equals the canvas width', () => {
		const row = Uint16Array.from([1, 2, 3, 4]);
		const out = sliceDensityRow(row, 4, 0, 1);
		expect([...out]).toEqual([1, 2, 3, 4]);
	});

	it('aggregates with MAX on the way down', () => {
		const row = Uint16Array.from([1, 9, 2, 8]);
		expect([...sliceDensityRow(row, 2, 0, 1)]).toEqual([9, 8]);
	});

	it('upscales by nearest column, never interpolating', () => {
		const row = Uint16Array.from([7, 0, 0, 3]);
		const out = sliceDensityRow(row, 8, 0, 1);
		expect([...out]).toEqual([7, 7, 0, 0, 0, 0, 3, 3]);
	});

	it('slices a sub-range of the capture (the zoomed view)', () => {
		// 8 columns across the capture; the view is the right half.
		const row = Uint16Array.from([1, 2, 3, 4, 5, 6, 7, 8]);
		const out = sliceDensityRow(row, 4, 0.5, 1);
		expect([...out]).toEqual([5, 6, 7, 8]);
	});

	it('handles rows stored at different widths (a resize mid-session)', () => {
		const narrow = Uint16Array.from([5, 1]);
		const wide = Uint16Array.from([1, 1, 1, 1, 1, 1, 1, 6]);
		// Same view fraction over both: the peak (5 vs 6) must surface either way.
		expect(sliceDensityRow(narrow, 4, 0, 1).reduce((a, b) => Math.max(a, b))).toBe(5);
		expect(sliceDensityRow(wide, 4, 0, 1).reduce((a, b) => Math.max(a, b))).toBe(6);
	});

	it('reuses the caller scratch buffer when it fits', () => {
		const scratch = new Uint16Array(4);
		const out = sliceDensityRow(Uint16Array.from([1, 2, 3, 4]), 4, 0, 1, scratch);
		expect(out).toBe(scratch);
	});
});
