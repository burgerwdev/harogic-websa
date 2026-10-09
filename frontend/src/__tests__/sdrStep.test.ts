/**
 * The per-bandwidth tuning step.
 *
 * The arrow keys tune by the step in the step box. The step is remembered per capture
 * bandwidth, because a step that walks a 50 MHz sweep is useless inside a 25 kHz channel.
 * These tests pin the rules the UI relies on: the span-derived default, the per-bandwidth
 * memory, the persistence, and the Preset reset.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import {
	SDR_STEP_QUICK_HZ, SDR_STEP_UNIT_FACTOR, currentSdrStepHz, defaultSdrStepHz,
	parseSdrStepMap, setSdrStepForCurrentBw, sdrDecimate, sdrSpanHz, sdrStepUnit,
	storedSdrStepHz,
} from '../ui/sdrState';

beforeEach(() => {
	localStorage.clear();
	resetAll('sdr');
});

describe('defaultSdrStepHz', () => {
	it('keeps the historical 1 kHz step for the 195 kHz capture span', () => {
		expect(defaultSdrStepHz(195_000)).toBe(1000);
	});

	it('narrows the step for narrow demodulation spans and widens it for wide ones', () => {
		expect(defaultSdrStepHz(24_400)).toBe(200); // 244 → the 2 of 1-2-5: SSB / CW work
		expect(defaultSdrStepHz(97_700)).toBe(500);
		expect(defaultSdrStepHz(1_560_000)).toBe(10_000);
		expect(defaultSdrStepHz(6_250_000)).toBe(50_000);
		expect(defaultSdrStepHz(12_500_000)).toBe(100_000);
		expect(defaultSdrStepHz(25_000_000)).toBe(200_000);
		expect(defaultSdrStepHz(50_000_000)).toBe(500_000);
	});

	it('always lands on a 1-2-5 value and never returns zero or a negative number', () => {
		for (let span = 100; span <= 60_000_000; span = Math.floor(span * 1.37) + 1) {
			const step = defaultSdrStepHz(span);
			expect(step).toBeGreaterThan(0);
			const mantissa = Number(step.toPrecision(2)) / step;
			expect([1, 2, 5]).toContain(mantissa);
		}
	});

	it('degrades safely on a missing or impossible span', () => {
		expect(defaultSdrStepHz(0)).toBe(1000);
		expect(defaultSdrStepHz(-1)).toBe(1000);
		expect(defaultSdrStepHz(NaN)).toBe(1000);
		expect(defaultSdrStepHz(Infinity)).toBe(1000);
		expect(defaultSdrStepHz(50)).toBe(1); // below the ladder floor
	});
});

describe('per-bandwidth step memory', () => {
	it('falls back to the span default until the user stores a step', () => {
		sdrSpanHz.set(195_000);
		expect(storedSdrStepHz(256)).toBeNull();
		expect(currentSdrStepHz()).toBe(1000); // the default, not a stored value
	});

	it('keeps one step per bandwidth and uses the stored one', () => {
		sdrSpanHz.set(0);
		sdrDecimate.set(256); // ~195 kHz span: default 1 kHz
		expect(currentSdrStepHz()).toBe(1000);
		setSdrStepForCurrentBw(2500);
		expect(currentSdrStepHz()).toBe(2500);

		sdrDecimate.set(2048); // ~24 kHz span, no stored step: its own default
		expect(storedSdrStepHz(2048)).toBeNull();
		expect(currentSdrStepHz()).toBe(200);

		sdrDecimate.set(2048);
		setSdrStepForCurrentBw(50);
		sdrDecimate.set(256);
		expect(currentSdrStepHz()).toBe(2500); // the 256x step came back unchanged
		sdrDecimate.set(2048);
		expect(currentSdrStepHz()).toBe(50);
	});

	it('ignores an impossible step', () => {
		sdrDecimate.set(256);
		setSdrStepForCurrentBw(0);
		setSdrStepForCurrentBw(-100);
		setSdrStepForCurrentBw(NaN);
		expect(storedSdrStepHz(256)).toBeNull();
	});

	it('persists across a reload and Preset clears it with the other preferences', () => {
		sdrDecimate.set(256);
		setSdrStepForCurrentBw(2500);
		expect(localStorage.getItem('web-sa-sdr-step-bw')).not.toBeNull();

		// The state module registers the key in SDR_PREF_KEYS, so the Preset path
		// (resetSdrState) removes it; here resetAll covers the in-memory side. With the
		// stored step gone, the default returns for the fallback decimation (32 = ~1.56 MHz).
		resetAll('sdr');
		localStorage.removeItem('web-sa-sdr-step-bw');
		expect(storedSdrStepHz(256)).toBeNull();
		expect(currentSdrStepHz()).toBe(10_000);
	});
});

describe('the stored step slot', () => {
	it('parses a stored map and drops entries that are not positive finite numbers', () => {
		const parsed = parseSdrStepMap(
			JSON.stringify({ 256: 2500, 512: -1, 1024: 'x', 2048: NaN, 4096: Infinity, 8192: 0 }));
		expect(parsed).toEqual({ 256: 2500 });
	});

	it('degrades corrupt storage to an empty map', () => {
		expect(parseSdrStepMap('{not json')).toEqual({});
		expect(parseSdrStepMap('null')).toEqual({});
		expect(parseSdrStepMap('')).toEqual({});
	});
});

describe('step unit and quick steps', () => {
	it('converts the box value to Hz', () => {
		expect(SDR_STEP_UNIT_FACTOR.Hz).toBe(1);
		expect(SDR_STEP_UNIT_FACTOR.kHz).toBe(1000);
		expect(SDR_STEP_UNIT_FACTOR.MHz).toBe(1e6);
	});

	it('offers the quick steps in Hz and persists the chosen unit', () => {
		expect(SDR_STEP_QUICK_HZ).toEqual([10, 100, 1000, 10_000, 100_000]);
		sdrStepUnit.set('MHz');
		expect(sdrStepUnit.get()).toBe('MHz');
		expect(localStorage.getItem('web-sa-sdr-step-unit')).toBe('MHz'); // serialize: String
		// Preset drops the intent too (authoritative only means no TTL expiry, not reset-proof).
		resetAll('sdr');
		expect(sdrStepUnit.get()).toBe('kHz');
	});
});
