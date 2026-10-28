/** Native density history, visible frequency raster, and persistence controls. */
import { beforeEach, describe, expect, it } from 'vitest';
import { advanceDensity, densityLevelPlacement, densityLut, paintDensity, DENSITY_HIT_RATE, MAX_DENSITY, rasterDensity, rasterSparseDensity } from '../dsp/rtaDensity';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { resetRtaDensityCfg, restoreRtaDensityCfg, setRtaBins, setRtaDensContrast, setRtaDensWidth, setRtaDensSource, setRtaHalflife } from '../ui/panels/rta';
import { clearRtaTrace, setTraceAverage, setTraceMode, switchTraceTab } from '../ui/traceOps';
import { rtaDensWidth, rtaDensSource, rtaHalflife } from '../ui/waterfallState';

const BINS = 64;
function specWithPeak(points = 32, peakDb = -60): Float32Array {
	const spec = new Float32Array(points).fill(-100);
	spec[Math.floor(points / 2)] = peakDb;
	return spec;
}

function densSelect(value = ''): HTMLSelectElement {
	document.body.innerHTML = `<select id="select-rta-fade">${value}</select>`;
	return document.getElementById('select-rta-fade') as HTMLSelectElement;
}

/** Reference-frame weight: what one 1/30 s hit adds under the calibrated rate. */
const FRAME_HIT = DENSITY_HIT_RATE / 30;

beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	localStorage.clear();
	S.setRtaDensity2d(null);
	S.setActiveTraceIdx(0);
	S.traces[0].mode = 'CLEAR_WRITE';
});

describe('native density history', () => {
	it('releases history for Off and does not mutate the previous frame', () => {
		const grid = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		const before = grid.values.slice();
		expect(advanceDensity(grid, specWithPeak(), BINS, 0, -20, 2)).toBeNull();
		expect(advanceDensity(grid, specWithPeak(), BINS, 1, -20, 2, 'OFF')).toBeNull();
		expect(grid.values).toEqual(before);
	});

	it('decays by real elapsed time, not frame count, and normalizes hits to it', () => {
		const spec = specWithPeak();
		// Frames 1 s apart under a 1 s half-life: the old hit halves, the new hit carries the
		// elapsed occupancy (rate x dt). Frames 33 ms apart: almost no decay and a small hit -
		// the steady-state level is time-based, so frame rate cannot change it.
		const g1 = advanceDensity(null, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
		advanceDensity(g1, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 1000);
		expect(g1.values[16 * BINS + 20]).toBeCloseTo(FRAME_HIT * 0.5 + DENSITY_HIT_RATE * 0.25, 4);
		const g2 = advanceDensity(null, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
		advanceDensity(g2, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 33);
		expect(g2.values[16 * BINS + 20]).toBeCloseTo(FRAME_HIT * Math.pow(2, -33 / 1000) + DENSITY_HIT_RATE * 0.033, 3);
		// The same second of observation converges to the same level whatever the frame
		// count is: 30 fps and 60 fps differ by well under the decay step.
		const run = (frames: number) => {
			const g = advanceDensity(null, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
			for (let i = 1; i <= frames; i++) advanceDensity(g, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', i * (1000 / frames));
			return g.values[16 * BINS + 20];
		};
		expect(Math.abs(run(30) - run(60)) / run(60)).toBeLessThan(0.02);
	});

	it('does not add occupancy for frames sharing a timestamp', () => {
		const spec = specWithPeak();
		const grid = advanceDensity(null, spec, BINS, 2, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 1000)!;
		const before = grid.values.slice();
		advanceDensity(grid, spec, BINS, 2, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 1000);
		expect(grid.values).toEqual(before);
	});

	it('bounds a tab-wake gap: no flood-in, no wipe, no invented occupancy', () => {
		const spec = specWithPeak();
		const grid = advanceDensity(null, spec, BINS, 2, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
		// 60 s of silence decays as 2 s (the clamp) and adds at most 0.25 s of hits.
		const before = grid.values[16 * BINS + 20];
		advanceDensity(grid, spec, BINS, 2, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 60000);
		const after = grid.values[16 * BINS + 20];
		expect(after).toBeGreaterThan(before * 0.24);
		expect(after).toBeLessThan(before * 0.25 + DENSITY_HIT_RATE * 0.25 + 1);
	});

	it('freezes VIEW without fading or adding repeated frozen samples', () => {
		const grid = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		const before = grid.values.slice();
		const revision = grid.revision;
		expect(advanceDensity(grid, specWithPeak(32, -40), BINS, 0.5, 10, 1, 'VIEW')).toBe(grid);
		expect(grid.values).toEqual(before);
		expect(grid.revision).toBe(revision);
		// A trace first selected while frozen still gets one density snapshot.
		const snapshot = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2, 'VIEW')!;
		expect(snapshot.values[16 * BINS + 20]).toBeGreaterThan(0);
	});

	it('rebuilds on native or amplitude geometry changes and never clamps an offscreen peak', () => {
		const old = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		const resized = advanceDensity(old, specWithPeak(16), BINS, 0.5, -20, 2)!;
		expect(resized.heads.length).toBe(16);
		const moved = advanceDensity(old, specWithPeak(), BINS, 1, -40, 1)!;
		expect(moved).not.toBe(old);
		expect(moved.values[16 * BINS + 20]).toBeGreaterThan(0);
		const clipped = advanceDensity(null, specWithPeak(32, 10), BINS, 1, -20, 1)!;
		expect(clipped.heads[16]).toBe(-1);
	});

	it('retains weak noise only at measured levels while an abandoned carrier decays', () => {
		const spec = specWithPeak();
		const grid = advanceDensity(null, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
		const peak = grid.values[16 * BINS + 20];
		const noise = new Float32Array(32).fill(-100);
		advanceDensity(grid, noise, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 1000);
		expect(grid.values[16 * BINS + 20]).toBeCloseTo(peak / 2, 5);
		expect(grid.values[16 * BINS + 40]).toBeGreaterThan(0);
		for (let bin = 0; bin < BINS; bin++) {
			if (bin !== 20 && bin !== 40) expect(grid.values[16 * BINS + bin]).toBe(0);
		}
		expect(noise).toEqual(new Float32Array(32).fill(-100));
	});

	it('saturates at the level the colour map uses', () => {
		const spec = specWithPeak(8, -20);
		const grid = advanceDensity(null, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', 0)!;
		for (let i = 1; i <= 200; i++) advanceDensity(grid, spec, BINS, 1, -20, 2, 'CLEAR_WRITE', undefined, 'trace', i * 1000);
		expect(Math.max(...grid.values)).toBe(MAX_DENSITY);
	});

	it('starts a new history when the source switches, and live ignores trace modes', () => {
		const spec = specWithPeak();
		const traceGrid = advanceDensity(null, spec, BINS, 1, -20, 2)!;
		// VIEW freezes only the trace-following history; the live source keeps accumulating.
		const liveGrid = advanceDensity(traceGrid, spec, BINS, 1, -20, 2, 'VIEW', undefined, 'live')!;
		expect(liveGrid).not.toBe(traceGrid);
		expect(liveGrid.source).toBe('live');
	});

	it('resolves neighbouring native carriers after zoom and keeps irregular Hz centres', () => {
		const freq = new Float64Array(16008);
		for (let i = 0; i < 16008; i++) freq[i] = 28e6 + i * 1000;
		const spec = new Float32Array(16008).fill(-100);
		spec[8000] = -40; spec[8001] = -40; spec[8002] = -70; spec[8003] = NaN;
		const grid = advanceDensity(null, spec, BINS, 1, 0, 2)!;
		const lo = freq[7998], hi = freq[8006], width = 800;
		const raster = new Float32Array(width * BINS);
		rasterDensity(grid, freq, lo, hi, width, raster);
		const xPeak1 = Math.floor((freq[8000] - lo) / (hi - lo) * width);
		const xPeak2 = Math.floor((freq[8001] - lo) / (hi - lo) * width);
		expect(xPeak2 - xPeak1).toBeGreaterThan(80);
	});

	it('renders an isolated finite native carrier surrounded by NaN gaps', () => {
		const freq = new Float64Array([0, 1, 2]);
		const grid = advanceDensity(null, new Float32Array([NaN, -40, NaN]), BINS, 1, 0, 2)!;
		const raster = new Float32Array(100 * BINS);
		rasterDensity(grid, freq, 0, 2, 100, raster);
		const pixels = new Uint8ClampedArray(100 * 64 * 4);
		paintDensity(raster, 100, BINS, pixels, 64, 0, 1, 1, 1, densityLut(false));
		expect(pixels[(20 * 100 + 50) * 4 + 3]).toBeGreaterThan(0);
		for (const x of [49, 51]) expect(pixels[(20 * 100 + x) * 4 + 3]).toBe(0);
	});

	it('keeps equal resolved samples equally bright across a half-pixel viewport pan', () => {
		const freq = Float64Array.from({ length: 16 }, (_, i) => i);
		const spec = new Float32Array(16).fill(-120);
		spec[6] = -40; spec[7] = -40; spec[8] = -40;
		const grid = advanceDensity(null, spec, BINS, 1, 0, 2, 'CLEAR_WRITE', freq)!;
		const boundary = new Float32Array(16 * BINS);
		const centred = new Float32Array(16 * BINS);
		rasterDensity(grid, freq, 0, 16, 16, boundary);
		rasterDensity(grid, freq, -0.5, 15.5, 16, centred);
		const value = grid.values[7 * BINS + 20];
		for (const x of [6, 7]) {
			expect(boundary[20 * 16 + x]).toBeCloseTo(value, 6);
			expect(centred[20 * 16 + x]).toBeCloseTo(value, 6);
		}
	});

	it('splits a fractional amplitude hit around its measured level and replaces a same-size axis', () => {
		const spec = specWithPeak();
		const grid = advanceDensity(null, spec, BINS, 1, -20, 2)!;
		const row = (-20 - -60) / 2;
		const bin = Math.floor(row), frac = row - bin;
		expect(grid.values[16 * BINS + bin]).toBeCloseTo(FRAME_HIT * (1 - frac), 5);
		expect(grid.values[16 * BINS + bin + 1]).toBeCloseTo(FRAME_HIT * frac, 5);
	});

	it('maps row centres with offset, reference and scale even without advancing history', () => {
		const grid = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		const p0 = densityLevelPlacement(grid, -10, 100, 0);
		const p1 = densityLevelPlacement(grid, -10, 100, 5);
		expect(p1.top - p0.top).toBeCloseTo(-5 / 100, 8);
	});
});

describe('sparse density distribution projection', () => {
	function history(columns: number[][]) {
		const grid = advanceDensity(null, new Float32Array(columns.length), BINS, 1, 0, 1)!;
		grid.values.fill(0);
		grid.heads.fill(-1);
		for (let i = 0; i < columns.length; i++) {
			for (let bin = 0; bin < columns[i].length; bin++) {
				if (!(columns[i][bin] > 0)) continue;
				const cell = i * BINS + bin;
				grid.values[cell] = columns[i][bin];
				grid.next[cell] = grid.heads[i];
				grid.heads[i] = cell;
			}
		}
		return grid;
	}
	const peak = (row: number, mass: number) => {
		const histogram = new Array<number>(BINS).fill(0);
		histogram[row] = mass;
		return histogram;
	};

	it.each([[8, 56], [56, 8]])('moves steep slopes from row %i to %i through midpoint occupancy', (from, to) => {
		const grid = history([peak(from, 4), peak(to, 8)]);
		const raster = new Float32Array(9 * BINS);
		// Native frequencies and the midpoint land exactly on pixel centres.
		rasterSparseDensity(grid, new Float64Array([0, 1]), -0.0625, 1.0625, 9, raster);
		for (let x = 1; x < 8; x++) {
			const row = from + (to - from) * x / 8;
			let total = 0;
			for (let bin = 0; bin < BINS; bin++) total += raster[bin * 9 + x];
			expect(total).toBeCloseTo(4 + x / 2, 6);
			// Each pixel covers six amplitude rows; even the shared edge rows stay lit.
			for (let bin = row - 3; bin <= row + 3; bin++) expect(raster[bin * 9 + x]).toBeGreaterThan(0);
		}
		expect(raster[32 * 9 + 4]).toBeCloseTo(1, 6);
		expect(raster[from * 9]).toBe(4);
		expect(raster[to * 9 + 8]).toBe(8);
		expect(raster[from * 9 + 1]).toBe(0);
	});

	it('paints a continuous steep slope when amplitude pitch exceeds the CSS footprint', () => {
		const cols = 9, rows = 320, pitch = 5, footprint = 2;
		const grid = history([peak(8, 4), peak(56, 8)]);
		const raster = new Float32Array(cols * rows);
		rasterSparseDensity(grid, new Float64Array([0, 1]), -0.0625, 1.0625, cols, raster,
			{ bins: rows, origin: 0, scale: pitch });
		const pixels = new Uint8ClampedArray(cols * rows * 4);
		paintDensity(raster, cols, rows, pixels, rows, 0.5, 1, footprint, 1, densityLut(false), true);
		// Each device column covers 30 screen rows, including all rows between samples.
		for (let x = 1; x < cols - 1; x++) {
			const centre = (8 + 6 * x) * pitch;
			for (let y = centre - 15; y <= centre + 15; y++) {
				expect(pixels[(y * cols + x) * 4 + 3]).toBeGreaterThan(0);
			}
		}
	});

	it('matches cumulative mass in bin order and splits fractional rows without losing total', () => {
		const left = peak(0, 1), right = peak(16, 4);
		left[BINS - 1] = 3;
		right[BINS - 1] = 4;
		const raster = new Float32Array(9 * BINS);
		rasterSparseDensity(history([left, right]), new Float64Array([0, 1]), -0.0625, 1.0625, 9, raster);
		// Quantile masses .25, .25, .5 traverse midpoint rows 8, 39.5, 63.
		expect(raster[8 * 9 + 4]).toBeCloseTo(0.75, 6);
		expect(raster[39 * 9 + 4]).toBeGreaterThan(0);
		expect(raster[40 * 9 + 4]).toBeGreaterThan(0);
		expect(raster[63 * 9 + 4]).toBeCloseTo(3, 6);
		let total = 0;
		for (let bin = 0; bin < BINS; bin++) total += raster[bin * 9 + 4];
		expect(total).toBeCloseTo(6, 6);
	});

	it.each(['NaN', 'empty'])('does not bridge a %s column and preserves isolated native points', gap => {
		const grid = history([peak(8, 4), gap === 'empty' ? [] : peak(32, 5), peak(56, 6)]);
		if (gap === 'NaN') grid.samples[1] = NaN;
		const raster = new Float32Array(21 * BINS);
		rasterSparseDensity(grid, new Float64Array([0, 1, 2]), 0, 2, 21, raster);
		expect(raster[8 * 21]).toBe(4);
		expect(raster[56 * 21 + 20]).toBe(6);
		for (let bin = 0; bin < BINS; bin++) {
			for (let x = 1; x < 20; x++) expect(raster[bin * 21 + x]).toBe(0);
		}
	});

	it('keeps steady histograms identical to the old raster and leaves raw history unchanged', () => {
		const histogram = peak(7, 3);
		histogram[29] = 1;
		const grid = history([histogram, histogram]);
		const values = grid.values.slice(), next = grid.next.slice(), heads = grid.heads.slice();
		const samples = grid.samples.slice(), revision = grid.revision;
		const oldRaster = new Float32Array(9 * BINS), sparse = new Float32Array(oldRaster.length);
		const freq = new Float64Array([0, 1]);
		rasterDensity(grid, freq, 0, 1, 9, oldRaster);
		rasterSparseDensity(grid, freq, 0, 1, 9, sparse);
		expect(sparse).toEqual(oldRaster);
		expect(grid.values).toEqual(values);
		expect(grid.next).toEqual(next);
		expect(grid.heads).toEqual(heads);
		expect(grid.samples).toEqual(samples);
		expect(grid.revision).toBe(revision);
	});

	it('max-composites native peaks sharing one pixel and never extrapolates endpoints', () => {
		const grid = history([peak(8, 4), peak(8, 6)]);
		const freq = new Float64Array([0, 1]);
		const subpixel = new Float32Array(BINS);
		rasterSparseDensity(grid, freq, 0, 1, 1, subpixel);
		expect(subpixel[8]).toBe(6);
		const window = new Float32Array(12 * BINS);
		rasterSparseDensity(grid, freq, -1, 2, 12, window);
		for (const x of [0, 1, 2, 3, 9, 10, 11]) expect(window[8 * 12 + x]).toBe(0);
	});
});

describe('density invalidation through trace controls', () => {
	const seed = () => {
		const grid = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		S.setRtaDensity2d(grid);
		return grid;
	};

	it('preserves VIEW but clears on resume, another mode, or a different active trace', () => {
		const frozen = seed();
		setTraceMode('VIEW');
		expect(S.rtaDensity2d).toBe(frozen);
		setTraceMode('MAX_HOLD');
		expect(S.rtaDensity2d).toBeNull();
		seed();
		setTraceMode('MIN_HOLD');
		expect(S.rtaDensity2d).toBeNull();
		seed();
		switchTraceTab(1);
		expect(S.rtaDensity2d).toBeNull();
	});

	it('clears on Clear, average depth, grain, source switch and persistence Off before another frame', () => {
		seed(); clearRtaTrace(); expect(S.rtaDensity2d).toBeNull();
		seed(); setTraceAverage(32); expect(S.rtaDensity2d).toBeNull();
		seed(); setRtaBins(128); expect(S.rtaDensity2d).toBeNull();
		seed(); setRtaDensSource('live'); expect(S.rtaDensity2d).toBeNull();
		expect(localStorage.getItem('rta-dens-source')).toBe('live');
		seed(); setRtaHalflife(0); expect(S.rtaDensity2d).toBeNull();
	});
});

describe('the persistence setting', () => {
	it('keeps Off when selected and when restored from storage', () => {
		densSelect('<option value="0">Off</option><option value="2">2 s</option>');
		setRtaHalflife(0);
		expect(rtaHalflife.get()).toBe(0);
		expect(localStorage.getItem('rta-halflife')).toBe('0');
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0');
		rtaHalflife.set(2);
		restoreRtaDensityCfg();
		expect(rtaHalflife.get()).toBe(0);
	});

	it('selects 0.2 s and restores that exact choice from storage', () => {
		densSelect('<option value="0">Off</option><option value="0.2">0.2 s</option><option value="0.5" selected>0.5 s</option>');
		setRtaHalflife(0.2);
		expect(rtaHalflife.get()).toBe(0.2);
		expect(localStorage.getItem('rta-halflife')).toBe('0.2');
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0.2');
		rtaHalflife.set(2);
		restoreRtaDensityCfg();
		expect(rtaHalflife.get()).toBe(0.2);
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0.2');
	});

	it('uses the panel default when storage is absent, a real gear for invalid input, and drops the old per-frame key', () => {
		localStorage.setItem('rta-fade', '0.975');
		const sel = densSelect('<option value="0">Off</option><option value="0.5" selected>0.5 s</option><option value="2">2 s</option>');
		restoreRtaDensityCfg();
		expect(rtaHalflife.get()).toBe(Number(sel.value));
		expect(localStorage.getItem('rta-fade')).toBeNull();
		setRtaHalflife(Number('not a number'));
		expect(rtaHalflife.get()).toBeGreaterThan(0);
		localStorage.setItem('rta-halflife', '0.975');
		restoreRtaDensityCfg();
		expect(rtaHalflife.get()).toBe(0.5);
		expect(sel.value).toBe('0.5');
	});

	it('restores the source into both the slot and the select', () => {
		document.body.innerHTML = '<select id="select-rta-dens-source"><option value="trace">Trace</option><option value="live">Live</option></select>';
		setRtaDensSource('live');
		rtaDensSource.set('trace');
		restoreRtaDensityCfg();
		expect(rtaDensSource.get()).toBe('live');
		expect((document.getElementById('select-rta-dens-source') as HTMLSelectElement).value).toBe('live');
	});
});

describe('density display footprint and visibility', () => {
	const alphas = (pixels: Uint8ClampedArray) => Array.from(pixels).filter((_, i) => i % 4 === 3);
	it('keeps widths in CSS pixels across amplitude pitches and pixel ratios', () => {
		for (const ratio of [1, 2]) for (const pitch of [2, 3, 7]) for (const width of [1, 3, 5, 9]) {
			const raster = new Float32Array([0, MAX_DENSITY, 0]);
			const pixels = new Uint8ClampedArray(80 * ratio * 4);
			const centre = 30.5 * ratio;
			paintDensity(raster, 1, 3, pixels, 80 * ratio, centre - pitch, pitch, width * ratio, 1, densityLut(false));
			const alpha = alphas(pixels);
			const fullAlpha = Math.round(255 * (1 - Math.exp(-5)));
			const mass = alpha.reduce((a, b) => a + b, 0);
			expect(mass / fullAlpha / ratio).toBeCloseTo(width, 1);
			expect(alpha.reduce((sum, a, y) => sum + a * (y + 0.5), 0) / mass / ratio).toBeCloseTo(30.5, 2);
			for (let y = 0; y < alpha.length; y++) {
				if (y + 1 <= centre - width * ratio / 2 || y >= centre + width * ratio / 2) expect(alpha[y]).toBe(0);
			}
			expect(raster).toEqual(new Float32Array([0, MAX_DENSITY, 0]));
		}
	});

	it('clips footprints at the plot boundary without moving their centres onto the edge', () => {
		const pixels = new Uint8ClampedArray(12 * 4);
		paintDensity(new Float32Array([MAX_DENSITY]), 1, 1, pixels, 12, -1, 3, 5, 1, densityLut(false));
		const alpha = alphas(pixels);
		expect(alpha[0]).toBeGreaterThan(200);
		expect(Math.abs(alpha[1] - alpha[0] / 2)).toBeLessThanOrEqual(1);
		expect(alpha.slice(2)).toEqual(new Array(10).fill(0));
		paintDensity(new Float32Array([MAX_DENSITY]), 1, 1, pixels, 12, -4, 3, 5, 1, densityLut(false));
		expect(alphas(pixels)).toEqual(new Array(12).fill(0));
	});

	it('feathers an isolated native carrier edge while keeping its flat centre and frequency gaps', () => {
		const freq = new Float64Array([0, 1, 2]);
		const grid = advanceDensity(null, new Float32Array([NaN, -40, NaN]), BINS, 1, 0, 2)!;
		for (const ratio of [1, 2]) for (const pitch of [2, 7]) {
			const cols = 100 * ratio, rows = 80 * ratio, centre = 30.5 * ratio, width = 5 * ratio;
			const raster = new Float32Array(cols * BINS);
			rasterDensity(grid, freq, 0, 2, cols, raster);
			const pixels = new Uint8ClampedArray(cols * rows * 4);
			paintDensity(raster, cols, BINS, pixels, rows, centre - 20 * pitch, pitch, width, 1, densityLut(false), true);
			const alpha = Array.from({ length: rows }, (_, y) => pixels[(y * cols + 50 * ratio) * 4 + 3]);
			const mass = alpha.reduce((sum, a) => sum + a, 0);
			expect(mass).toBeGreaterThan(0);
			expect(alpha.reduce((sum, a, y) => sum + a * (y + 0.5), 0) / mass / ratio).toBeCloseTo(30.5, 2);
			const edge = Math.floor(centre - width / 2);
			expect(alpha[Math.floor(centre)]).toBeGreaterThan(alpha[edge]);
			const centreAlpha = Math.round(255 * (1 - Math.exp(-5 * Math.pow(grid.values[BINS + 20] / MAX_DENSITY, 0.4))));
			expect(alpha[Math.floor(centre)]).toBe(centreAlpha);
			expect(alpha[Math.floor(centre) - 1]).toBe(centreAlpha);
			for (let y = 0; y < rows; y++) {
				if (y + 1 <= centre - width / 2 || y >= centre + width / 2) expect(alpha[y]).toBe(0);
				for (const x of [50 * ratio - 1, 50 * ratio + 1]) expect(pixels[(y * cols + x) * 4 + 3]).toBe(0);
			}
			expect(grid.samples[1]).toBe(-40);
			expect(raster[20 * cols + 50 * ratio]).toBeCloseTo(grid.values[BINS + 20]);
		}
	});

	it('clips the soft native footprint using the original centre and finite edges', () => {
		const freq = new Float64Array([0, 1, 2]);
		const grid = advanceDensity(null, new Float32Array([NaN, -40, NaN]), BINS, 1, 0, 2)!;
		const cols = 100, rows = 12, pitch = 3;
		const raster = new Float32Array(cols * BINS);
		rasterDensity(grid, freq, 0, 2, cols, raster);
		const full = new Uint8ClampedArray(cols * rows * 4), clipped = new Uint8ClampedArray(full.length);
		paintDensity(raster, cols, BINS, full, rows, 8.5 - 20 * pitch, pitch, 5, 1, densityLut(false), true);
		paintDensity(raster, cols, BINS, clipped, rows, -0.5 - 20 * pitch, pitch, 5, 1, densityLut(false), true);
		expect(clipped[(0 * cols + 50) * 4 + 3]).toBeGreaterThan(clipped[(1 * cols + 50) * 4 + 3]);
		for (let y = 0; y < rows; y++) {
			const expected = y + 9 < rows ? full[((y + 9) * cols + 50) * 4 + 3] : 0;
			expect(clipped[(y * cols + 50) * 4 + 3]).toBe(expected);
			if (y >= 2) expect(clipped[(y * cols + 50) * 4 + 3]).toBe(0);
		}
		paintDensity(raster, cols, BINS, clipped, rows, -3 - 20 * pitch, pitch, 5, 1, densityLut(false), true);
		expect(clipped.every(value => value === 0)).toBe(true);
	});

	it('makes rare cells visible in both themes with contrast independent of opacity', () => {
		const raster = new Float32Array([0, 0.08, 1, MAX_DENSITY]);
		const before = raster.slice();
		let previous: number[] | undefined;
		for (const light of [false, true]) for (const contrast of [0.6, 1, 1.6]) {
			const pixels = new Uint8ClampedArray(4 * 4);
			paintDensity(raster, 4, 1, pixels, 1, 0.5, 3, 1, contrast, densityLut(light));
			const alpha = alphas(pixels);
			expect(alpha[0]).toBe(0);
			expect(alpha[1]).toBeGreaterThan(70);
			expect(alpha[2]).toBeGreaterThan(alpha[1]);
			expect(alpha[3]).toBeGreaterThan(alpha[2]);
			if (previous) expect(alpha).toEqual(previous);
			previous = alpha;
			// Even a rare tail must visibly separate from the theme background after blending.
			const bg = light ? 250 : 15;
			const distance = Math.hypot(...Array.from(pixels.slice(4, 7), c => (c - bg) * alpha[1] / 255));
			expect(distance).toBeGreaterThan(40);
		}
		expect(raster).toEqual(before);
	});

	it('preserves history, source and measured samples when width or contrast changes, and resets width on Preset', () => {
		const samples = specWithPeak();
		const original = samples.slice();
		const grid = advanceDensity(null, samples, BINS, 2, -20, 2)!;
		const values = grid.values.slice();
		S.setRtaDensity2d(grid);
		setRtaDensWidth(9); setRtaDensContrast(0.6);
		expect(S.rtaDensity2d).toBe(grid);
		expect(grid.values).toEqual(values);
		expect(samples).toEqual(original);
		expect(rtaDensSource.get()).toBe('trace');
		expect(rtaDensWidth.get()).toBe(9);
		resetRtaDensityCfg();
		expect(rtaDensWidth.get()).toBe(5);
	});
});
