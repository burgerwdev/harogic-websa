/** Native density history, visible frequency raster, and persistence controls. */
import { beforeEach, describe, expect, it } from 'vitest';
import { advanceDensity, densityLevelPlacement, MAX_DENSITY, rasterDensity } from '../dsp/rtaDensity';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { restoreRtaDensityCfg, setRtaBins, setRtaFade } from '../ui/panels/rta';
import { clearRtaTrace, setTraceAverage, setTraceMode, switchTraceTab } from '../ui/traceOps';
import { rtaFade } from '../ui/waterfallState';

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

	it('decays old hits, adds the current hit, and eventually removes abandoned cells', () => {
		const spec = specWithPeak();
		const grid = advanceDensity(null, spec, BINS, 1, -20, 2)!;
		const k = 16 * BINS + 20;
		const once = grid.values[k];
		advanceDensity(grid, spec, BINS, 0.5, -20, 2);
		expect(grid.values[k]).toBeCloseTo(once * 1.5, 6);
		const noise = new Float32Array(32).fill(-100);
		for (let i = 0; i < 20; i++) advanceDensity(grid, noise, BINS, 0.5, -20, 2);
		expect(grid.values[k]).toBe(0);
		expect(grid.heads[16]).toBe(-1);
		advanceDensity(grid, spec, BINS, 1, -20, 2);
		expect(grid.values[k]).toBeCloseTo(once, 6);
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

	it('saturates at the level the colour map uses', () => {
		const spec = specWithPeak(8, -20);
		const grid = advanceDensity(null, spec, BINS, 1, -20, 2)!;
		for (let i = 0; i < 200; i++) advanceDensity(grid, spec, BINS, 1, -20, 2);
		expect(Math.max(...grid.values)).toBe(MAX_DENSITY);
	});

	it('resolves neighbouring native carriers after zoom and keeps irregular Hz centres', () => {
		const freq = Float64Array.from({ length: 16240 }, (_, i) => 1e9 + i * 7);
		freq[8002] += 1;
		const spec = new Float32Array(freq.length).fill(-120);
		spec[8001] = -40; spec[8002] = -70; spec[8003] = NaN;
		const grid = advanceDensity(null, spec, BINS, 1, 0, 2)!;
		const lo = freq[7998], hi = freq[8006], width = 800;
		const raster = new Float32Array(width * BINS);
		rasterDensity(grid, freq, lo, hi, width, raster);
		for (const [index, row] of [[8001, 20], [8002, 35]]) {
			const x = Math.floor((freq[index] - lo) / (hi - lo) * width);
			expect(Math.max(raster[row * width + Math.max(0, x - 1)], raster[row * width + x])).toBeGreaterThan(0);
		}
		const gapX = Math.floor((freq[8003] - lo) / (hi - lo) * width);
		expect(raster[20 * width + gapX]).toBe(0);
		expect(raster[35 * width + gapX]).toBe(0);
	});
	it('tapers wide native samples continuously without a rectangular bin or a filled gap', () => {
		const freq = Float64Array.from([0, 100, 200, 300, 400]);
		const spec = Float32Array.from([-120, -40, -120, NaN, -120]);
		const grid = advanceDensity(null, spec, BINS, 1, 0, 2, 'CLEAR_WRITE', freq)!;
		const width = 400;
		const out = new Float32Array(width * BINS);
		rasterDensity(grid, freq, 0, 400, width, out);
		const at = (x: number) => out[20 * width + x];
		expect(at(25)).toBeGreaterThan(0);
		expect(at(25)).toBeLessThan(at(50));
		expect(at(50)).toBeLessThan(at(75));
		expect(at(100)).toBeCloseTo(grid.values[BINS + 20] * 0.995, 6);
		expect(at(125)).toBeGreaterThan(at(150));
		expect(at(150)).toBeGreaterThan(at(175));
		expect(at(200)).toBe(0);
		expect(at(300)).toBe(0);
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
		const freq = Float64Array.from({ length: 32 }, (_, i) => i * 100);
		const grid = advanceDensity(null, specWithPeak(32, -60.5), BINS, 1, -20, 2, 'CLEAR_WRITE', freq)!;
		const base = 16 * BINS;
		const lower = grid.values[base + 20], upper = grid.values[base + 21];
		expect(upper / (lower + upper)).toBeCloseTo(0.25, 6);
		expect(grid.values[base + 19]).toBe(0);
		expect(grid.values[base + 22]).toBe(0);
		const moved = Float64Array.from(freq, hz => hz + 10000);
		const next = advanceDensity(grid, specWithPeak(32, -80), BINS, 1, -20, 2, 'VIEW', moved)!;
		expect(next).not.toBe(grid);
		expect(next.values[base + 20]).toBe(0);
		expect(next.values[base + 30]).toBeGreaterThan(0);
	});

	it('maps row centres with offset, reference and scale even without advancing history', () => {
		const grid = advanceDensity(null, specWithPeak(), BINS, 1, -20, 2)!;
		for (const [ref, range, offset] of [[-20, 128, 0], [-30, 80, 7], [-40, 160, -12]]) {
			const placement = densityLevelPlacement(grid, ref, range, offset);
			const row = 20;
			const centre = placement.top + (row + 0.5) / BINS * placement.height;
			expect(centre).toBeCloseTo((ref - (-60 + offset)) / range, 12);
		}
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

	it('clears on Clear, average depth, grain, and persistence Off before another frame', () => {
		seed(); clearRtaTrace(); expect(S.rtaDensity2d).toBeNull();
		seed(); setTraceAverage(32); expect(S.rtaDensity2d).toBeNull();
		seed(); setRtaBins(128); expect(S.rtaDensity2d).toBeNull();
		seed(); setRtaFade(0); expect(S.rtaDensity2d).toBeNull();
	});
});

describe('the persistence setting', () => {
	it('keeps Off when selected and when restored from storage', () => {
		densSelect('<option value="0">Off</option><option value="0.975">Medium</option>');
		setRtaFade(0);
		expect(rtaFade.get()).toBe(0);
		expect(localStorage.getItem('rta-fade')).toBe('0');
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0');
		rtaFade.set(0.975);
		restoreRtaDensityCfg();
		expect(rtaFade.get()).toBe(0);
	});

	it('uses the panel default when storage is absent and a real gear for invalid input', () => {
		const sel = densSelect('<option value="0">Off</option><option value="0.975" selected>Medium</option>');
		restoreRtaDensityCfg();
		expect(rtaFade.get()).toBe(Number(sel.value));
		setRtaFade(Number('not a number'));
		expect(rtaFade.get()).toBeGreaterThan(0);
	});
});
