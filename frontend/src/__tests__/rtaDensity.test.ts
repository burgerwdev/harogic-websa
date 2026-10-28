/** Native density history, visible frequency raster, and persistence controls. */
import { beforeEach, describe, expect, it } from 'vitest';
import { advanceDensity, densityLevelPlacement, densityLut, paintDensity, DENSITY_HIT_RATE, MAX_DENSITY, rasterDensity, rasterDensityContours, SDR_DENSITY_HISTORY_BYTES, SDR_DENSITY_HISTORY_FRAMES, type DensityGrid } from '../dsp/rtaDensity';
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

describe('SDR measured contour persistence', () => {
	const advance = (previous: DensityGrid | null, samples: Float32Array,
		freq: Float64Array, now: number, half = 1, mode = 'CLEAR_WRITE', source: 'trace' | 'live' = 'trace') =>
		advanceDensity(previous, samples, 64, half, 0, 1, mode, freq, source, now, true)!;
	const raster = (grid: DensityGrid, freq: Float64Array,
		lo: number, hi: number, cols: number, rows = 64, centre = 0, pitch = 1, width = 1) => {
		const out = new Float32Array(cols * rows);
		rasterDensityContours(grid, freq, lo, hi, cols, rows, centre, pitch, width, out, new Float32Array(rows));
		return out;
	};

	it('joins measured slopes at zoomed intermediate amplitudes instead of horizontal native bricks', () => {
		const freq = new Float64Array([0, 10]);
		const grid = advance(null, new Float32Array([-10, -30]), freq, 0);
		const out = raster(grid, freq, 0, 10, 100);
		for (const x of [20, 50, 80]) {
			const y = Math.floor(10 + 20 * (x + 0.5) / 100);
			expect(out[y * 100 + x] + out[(y + 1) * 100 + x]).toBeGreaterThan(0);
		}
		expect(out[10 * 100 + 50]).toBe(0);
		expect(out[30 * 100 + 50]).toBe(0);
	});

	it('preserves NaN gaps and isolated measured carriers in dense screen columns', () => {
		const freq = new Float64Array([0, 1, 2, 3, 4]);
		const grid = advance(null, new Float32Array([-30, -10, NaN, -20, -30]), freq, 0);
		const out = raster(grid, freq, 0, 4, 80);
		for (let y = 0; y < 64; y++) expect(out[y * 80 + 40]).toBe(0);
		const denseFreq = Float64Array.from({ length: 1000 }, (_, i) => i);
		const dense = new Float32Array(1000).fill(-40);
		dense[501] = -5; dense[502] = -60;
		const denseGrid = advance(null, dense, denseFreq, 0);
		const narrow = raster(denseGrid, denseFreq, 0, 1000, 10);
		expect(narrow[5 * 10 + 5]).toBeGreaterThan(0);
		expect(narrow[59 * 10 + 5]).toBeGreaterThan(0);
	});

	it('copies only the selected source and reprojects frozen contours with pixel width and bounds', () => {
		const freq = new Float64Array([0, 1]);
		const samples = new Float32Array([-20, -20]);
		const grid = advance(null, samples, freq, 0);
		samples.fill(-40);
		const original = raster(grid, freq, 0, 1, 10, 80, 0.5, 1, 1);
		const revision = grid.revision;
		expect(advance(grid, samples, freq, 1000, 1, 'VIEW')).toBe(grid);
		expect(grid.revision).toBe(revision);
		expect(raster(grid, freq, 0, 1, 10, 80, 0.5, 1, 1)).toEqual(original);
		const moved = raster(grid, freq, 0, 1, 10, 80, -9.5, 1, 5);
		expect(moved[10 * 10 + 5]).toBeGreaterThan(0);
		expect(moved[20 * 10 + 5]).toBe(0);
		const columnMass = (out: Float32Array) => out.reduce((sum, value, i) => sum + (i % 10 === 5 ? value : 0), 0);
		expect(columnMass(moved) / columnMass(original)).toBeCloseTo(5, 5);
		const live = advance(grid, samples, freq, 1100, 1, 'VIEW', 'live');
		expect(live).not.toBe(grid);
		expect(live.history!.frames[0].samples[0]).toBe(-40);
		expect(advanceDensity(live, samples, 64, 0, 0, 1, 'VIEW', freq, 'live', 1200, true)).toBeNull();
		expect(advanceDensity(live, samples, 64, 1, 0, 1, 'OFF', freq, 'trace', 1200, true)).toBeNull();
		expect(advance(live, samples, new Float64Array([0, 2]), 1200, 1, 'CLEAR_WRITE', 'live')).not.toBe(live);
	});

	it('halves abandoned contour occupancy by elapsed time and discards its faded tail', () => {
		const freq = new Float64Array([0, 1]);
		const grid = advance(null, new Float32Array([-10, -10]), freq, 0);
		const before = raster(grid, freq, 0, 1, 10, 64, 0.5);
		advance(grid, new Float32Array([-30, -30]), freq, 1000);
		const after = raster(grid, freq, 0, 1, 10, 64, 0.5);
		expect(after[10 * 10 + 5]).toBeCloseTo(before[10 * 10 + 5] / 2, 5);
		expect(after[30 * 10 + 5]).toBeGreaterThan(0);
		for (let i = 2; i <= 10; i++) advance(grid, new Float32Array([-30, -30]), freq, i * 1000);
		expect(raster(grid, freq, 0, 1, 10, 64, 0.5)[10 * 10 + 5]).toBe(0);
	});

	it('retains one contour for a native spectrum larger than the usual history budget', () => {
		const samples = new Float32Array(SDR_DENSITY_HISTORY_BYTES / 4 + 1).fill(-20);
		const grid = advanceDensity(null, samples, 64, 1, 0, 1, 'CLEAR_WRITE', undefined, 'trace', 0, true)!;
		expect(grid.history!.capacity).toBe(1);
		expect(grid.history!.frames[0].samples.byteLength).toBe(samples.byteLength);
		advanceDensity(grid, samples, 64, 1, 0, 1, 'CLEAR_WRITE', undefined, 'trace', 100, true);
		expect(grid.history!.frames.length).toBe(1);
	});

	it('keeps occupancy mass stable between full-view and zoom temporal sampling budgets', () => {
		const freq = Float64Array.from({ length: 12990 }, (_, i) => i);
		const samples = new Float32Array(freq.length).fill(-20);
		const grid = advance(null, samples, freq, 0, 10);
		for (let i = 1; i < 100; i++) advance(grid, samples, freq, i, 10);
		const full = raster(grid, freq, 0, 12989, 100, 64, 0.5);
		const zoom = raster(grid, freq, 6400, 6500, 100, 64, 0.5);
		for (let x = 1; x < 99; x++) {
			expect(full[20 * 100 + x]).toBeGreaterThan(0);
			expect(full[20 * 100 + x]).toBeCloseTo(zoom[20 * 100 + x], 5);
		}
		expect(grid.history!.frames.length).toBe(100);
	});

	it('compacts real spectra within a fixed budget while retaining long half-life mass', () => {
		const freq = Float64Array.from({ length: 12990 }, (_, i) => i);
		const samples = new Float32Array(freq.length).fill(-20);
		const grid = advance(null, samples, freq, 0, 10);
		let expected = DENSITY_HIT_RATE / 30;
		for (let i = 1; i <= 400; i++) {
			samples.fill(-20 - i); samples[0] = -i;
			advance(grid, samples, freq, i * 100, 10);
			expected = expected * Math.pow(2, -0.1 / 10) + DENSITY_HIT_RATE * 0.1;
		}
		const frames = grid.history!.frames;
		expect(frames.length).toBeLessThanOrEqual(SDR_DENSITY_HISTORY_FRAMES);
		expect(frames.reduce((sum, frame) => sum + frame.samples.byteLength, 0)).toBeLessThanOrEqual(SDR_DENSITY_HISTORY_BYTES);
		expect(frames.reduce((sum, frame) => sum + frame.weight, 0)).toBeCloseTo(expected, 5);
		expect(grid.values.byteLength + grid.next.byteLength + grid.heads.byteLength).toBe(0);
		for (const frame of frames) {
			// Every retained contour is one actual input frame, never averaged amplitudes.
			expect(Number.isInteger(frame.samples[0])).toBe(true);
			expect(frame.samples[1] === -20 || frame.samples[1] === frame.samples[0] - 20).toBe(true);
		}
		const before = raster(grid, freq, 5000, 5100, 100, 64);
		advance(grid, samples, freq, 40000, 10);
		expect(raster(grid, freq, 5000, 5100, 100, 64)).toEqual(before);
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
