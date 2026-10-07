/**
 * The RTA / SDR density grid and its persistence setting.
 *
 * Two claims are pinned here. Off really means off - the grid is neither advanced nor kept, which
 * is what takes the layer off the canvas and also skips the bins x points loop the display
 * otherwise pays for on every frame. And Off survives the round trip through the panel and the
 * browser storage: the handler this replaced wrote `parseFloat(v) || 0.98`, so Off silently came
 * back as "about Medium" and the option did nothing.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { advanceDensity, MAX_DENSITY } from '../dsp/rtaDensity';
import { resetAll } from '../core/params';
import { restoreRtaDensityCfg, setRtaFade } from '../ui/panels/rta';
import { rtaFade } from '../ui/waterfallState';

const BINS = 64;
/** Noise floor at -100 dBm with one 30 dB peak: the shape the weight rule is written for. */
function specWithPeak(points = 32, peakDb = -60): Float32Array {
	const spec = new Float32Array(points).fill(-100);
	spec[Math.floor(points / 2)] = peakDb;
	return spec;
}

function densSelect(value = ''): HTMLSelectElement {
	document.body.innerHTML =
		`<select id="select-rta-fade">${value}</select>`;
	return document.getElementById('select-rta-fade') as HTMLSelectElement;
}

beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	localStorage.clear();
});

describe('the density grid', () => {
	it('is not advanced and not kept while persistence is off', () => {
		const spec = specWithPeak();
		const prev = new Float32Array(spec.length * BINS);
		prev[0] = 5;
		expect(advanceDensity(prev, spec, BINS, 0, -20, 10)).toBeNull();
		expect(prev[0]).toBe(5);                       // the caller's grid is left untouched
	});

	it('accumulates a frame onto the level bin the sample falls in', () => {
		const spec = specWithPeak(32, -60);
		const grid = advanceDensity(null, spec, BINS, 0.975, -20, 10)!;
		expect(grid).toBeInstanceOf(Float32Array);
		expect(grid.length).toBe(spec.length * BINS);
		// refTop -20, 10 dB per bin: -100 dBm lands in bin 8, the peak in bin 4.
		const noiseBin = 8 * 32 + 0;                   // (col, bin) is column-major: col * bins + bin
		const peakCol = Math.floor(spec.length / 2);
		const peakBin = peakCol * BINS + 4;
		expect(grid[peakBin]).toBeGreaterThan(0);
		expect(grid[noiseBin]).toBe(0);                // the floor ripple (-100) stays out (rel < 3 dB)
	});

	it('decays the previous frame before adding the new one', () => {
		const spec = specWithPeak(32, -60);
		const first = advanceDensity(null, spec, BINS, 1, -20, 10)!;
		const col = Math.floor(spec.length / 2) * BINS + 4;
		const once = first[col];
		const second = advanceDensity(first, spec, BINS, 0.5, -20, 10)!;
		expect(second[col]).toBeCloseTo(once * 0.5 + once, 6);
	});

	it('starts a fresh grid when the geometry changed instead of decaying the old one', () => {
		const spec = specWithPeak(32, -60);
		const first = advanceDensity(null, spec, BINS, 1, -20, 10)!;
		const wider = advanceDensity(first, specWithPeak(16, -60), BINS, 0.5, -20, 10)!;
		expect(wider.length).toBe(16 * BINS);          // not the 32-point grid
	});

	it('saturates at the level the colour map is scaled to', () => {
		const spec = specWithPeak(8, -20);             // right at refTop: the hottest bin
		let grid = advanceDensity(null, spec, BINS, 1, -20, 10)!;
		for (let i = 0; i < 200; i++) grid = advanceDensity(grid, spec, BINS, 1, -20, 10)!;
		expect(Math.max(...grid)).toBe(MAX_DENSITY);
	});
});

describe('the persistence setting', () => {
	it('keeps Off when the panel selects it', () => {
		densSelect('<option value="0">Off</option><option value="0.975">Medium</option>');
		setRtaFade(0);
		expect(rtaFade.get()).toBe(0);
		expect(localStorage.getItem('rta-fade')).toBe('0');
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0');
	});

	it('restores Off on the way back in', () => {
		localStorage.setItem('rta-fade', '0');
		densSelect('<option value="0">Off</option><option value="0.975" selected>Medium</option>');
		restoreRtaDensityCfg();
		expect(rtaFade.get()).toBe(0);
		expect((document.getElementById('select-rta-fade') as HTMLSelectElement).value).toBe('0');
	});

	it('agrees with the panel when nothing was ever stored', () => {
		const sel = densSelect('<option value="0">Off</option><option value="0.975" selected>Medium</option>');
		restoreRtaDensityCfg();
		expect(rtaFade.get()).toBe(Number(sel.value));
		expect(rtaFade.get()).toBeGreaterThan(0);
	});

	it('falls back to a real gear when the value cannot be read', () => {
		densSelect('<option value="0.99">Slow</option>');
		setRtaFade(Number('not a number'));
		expect(rtaFade.get()).toBeGreaterThan(0);      // never silently turns the layer off
	});
});
