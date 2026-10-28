import { describe, expect, it } from 'vitest';
import { DensityRelease } from '../dsp/densityRelease';
import { advanceDensity, densityLut, paintDensity, rasterSparseDensity } from '../dsp/rtaDensity';

describe('fixed-coordinate density release', () => {
	it('releases sudden zero input by elapsed half-life without accumulating steady input', () => {
		const release = new DensityRelease();
		const raw = new Float32Array([8]);
		const storage = release.apply(raw, 0, 0.2);
		expect(release.apply(raw, 100, 0.2)).toBe(storage);
		expect(storage[0]).toBe(8);
		raw[0] = 0;
		let previous = storage[0];
		for (const time of [150, 200, 250, 300]) {
			const value = release.apply(raw, time, 0.2)[0];
			expect(value).toBeCloseTo(previous * Math.pow(2, -0.05 / 0.2), 6);
			expect(release.apply(raw, time, 0.2)[0]).toBe(value);
			previous = value;
		}
		expect(storage[0]).toBeCloseTo(4, 6);
		// A tab wake uses the same 0..2 s elapsed-time bound as native density.
		expect(release.apply(raw, 10300, 1)[0]).toBeCloseTo(1, 6);
	});

	it('copies raw on geometry reset or size replacement and releases storage on clear', () => {
		const release = new DensityRelease();
		release.apply(new Float32Array([8, 4]), 0, 1);
		expect(release.apply(new Float32Array([0, 2]), 100, 1, true)).toEqual(new Float32Array([0, 2]));
		expect(release.apply(new Float32Array([0, 0, 1]), 200, 1)).toEqual(new Float32Array([0, 0, 1]));
		release.clear();
		expect(release.values.length).toBe(0);
		expect(release.apply(new Float32Array([0, 0, 0]), 300, 1)).toEqual(new Float32Array(3));
	});

	it('keeps the painted old ROI fading when one native histogram crosses the deletion threshold', () => {
		const cols = 9, rows = 64;
		const freq = new Float64Array([0, 1]);
		const grid = advanceDensity(null, new Float32Array([-8, -56]), rows, 1, 0, 1,
			'CLEAR_WRITE', freq, 'trace', 0)!;
		grid.values.fill(0);
		grid.heads.fill(-1);
		for (const [column, row, value] of [[0, 8, 4], [1, 56, 0.051]]) {
			const cell = column * rows + row;
			grid.values[cell] = value;
			grid.heads[column] = cell;
			grid.next[cell] = -1;
		}
		const raw = new Float32Array(cols * rows);
		const release = new DensityRelease();
		const pixels = new Uint8ClampedArray(cols * rows * 4);
		const raster = () => rasterSparseDensity(grid, freq, -0.0625, 1.0625, cols, raw);
		raster();
		release.apply(raw, grid.lastMs, 1);
		paintDensity(release.values, cols, rows, pixels, rows, 0.5, 1, 1, 1, densityLut(false), true);
		const roi = 32 * cols + 4;
		const before = release.values[roi];
		const alpha = pixels[roi * 4 + 3];
		expect(before).toBeGreaterThan(0);
		expect(alpha).toBeGreaterThan(0);
		// Keep finite sample topology, but put new hits outside the amplitude grid.
		advanceDensity(grid, new Float32Array([-200, -200]), rows, 1, 0, 1,
			'CLEAR_WRITE', freq, 'trace', 100);
		expect(grid.heads[1]).toBe(-1);
		raster();
		expect(raw[roi]).toBe(0);
		release.apply(raw, grid.lastMs, 1);
		paintDensity(release.values, cols, rows, pixels, rows, 0.5, 1, 1, 1, densityLut(false), true);
		expect(release.values[roi]).toBeCloseTo(before * Math.pow(2, -0.1), 6);
		expect(pixels[roi * 4 + 3]).toBeGreaterThan(0);
		expect(pixels[roi * 4 + 3]).toBeLessThanOrEqual(alpha);
		advanceDensity(grid, new Float32Array([-200, -200]), rows, 1, 0, 1,
			'CLEAR_WRITE', freq, 'trace', 1000);
		raster();
		release.apply(raw, grid.lastMs, 1);
		expect(release.values[roi]).toBeCloseTo(before / 2, 6);
	});
});
