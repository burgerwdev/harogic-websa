/**
 * The viewport-sliced waterfall (render/waterfall.ts phase 2).
 *
 * Pins the storage/render split: rows keep their stored width; the render maps any row
 * onto the canvas through the visible capture fraction — MAX aggregation on the way down
 * (the rows' own peak-hold semantics), nearest column on the way up (a step, never an
 * interpolation), and a row-width floor/cap that decouples history from the canvas size.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
	setWaterfallRowWidth,
	sliceDensityRow,
	waterfallRowWidth,
} from '../render/waterfall';

beforeEach(() => {
	// back to the clamped default
	setWaterfallRowWidth(0);   // invalid: ignored, keeps the previous value
});

describe('row width floor and cap', () => {
	it('clamps into [2048, 16384]', () => {
		expect(waterfallRowWidth()).toBeGreaterThanOrEqual(2048);
		setWaterfallRowWidth(500);
		expect(waterfallRowWidth()).toBe(2048);       // the floor is what small screens get
		setWaterfallRowWidth(5000);
		expect(waterfallRowWidth()).toBe(5000);       // a 5k-wide canvas keeps 1:1
		setWaterfallRowWidth(99999);
		expect(waterfallRowWidth()).toBe(16384);      // the cap bounds memory on 8K
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
