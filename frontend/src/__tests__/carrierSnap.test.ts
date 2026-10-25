/**
 * Carrier evidence from raw spectrum frames.
 *
 * These tests pin the snap rules the SDR click relies on: raw frames are the only
 * evidence, persistence spans multiple frames in time, one-frame transients never
 * pass, the nearest candidate wins over a stronger distant one, staleness and
 * geometry changes reset the evidence, and ambiguous rivals are refused.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
	CARRIER_PROMINENCE_DB,
	CARRIER_STALE_MS,
	findCarrierNear,
	currentCarrierEvidence,
	observeCarrierSpectrum,
	resetCarrierEvidence,
} from '../dsp/carrierSnap';

const ID = 1;
const POINTS = 64;
const STEP_HZ = 1000;
const START_HZ = 100_000;
const MAX_BIN_HZ = 1000;

function axis(): Float64Array {
	const freq = new Float64Array(POINTS);
	for (let i = 0; i < POINTS; i++) freq[i] = START_HZ + i * STEP_HZ;
	return freq;
}

/** Flat noise floor with an optional carrier spike at `bin` of `promDb`. */
function frameWithCarrier(bin: number, promDb: number): Float32Array {
	const powers = new Float32Array(POINTS).fill(-100);
	powers[bin] = -100 + promDb;
	return powers;
}

function flatNoise(promDb = 2): Float32Array {
	return new Float32Array(POINTS).fill(-100 + promDb);
}

const near = (hz: number, now: number, radiusHz = 20_000, maxBinHz = MAX_BIN_HZ) =>
	findCarrierNear({ hz, radiusHz, maxBinHz, now });

beforeEach(() => {
	resetCarrierEvidence();
});

describe('persistence across multiple frames', () => {
	it('needs several distinct frames spanning the minimum time', () => {
		const freq = axis();
		const carrierBin = 30;
		// Only two frames so far: not enough even at a real span.
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 200, ID);
		expect(near(START_HZ + carrierBin * STEP_HZ, 200).ok).toBe(false);
		// Three frames, but the whole span is under the minimum.
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 100, ID);
		expect(near(START_HZ + carrierBin * STEP_HZ, 100).ok).toBe(false);
		// Reset, then three frames spanning past the minimum: accepted.
		resetCarrierEvidence();
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(carrierBin, 20), 120, ID);
		const r = near(START_HZ + carrierBin * STEP_HZ, 120);
		expect(r.ok).toBe(true);
		if (r.ok) {
			expect(r.candidate.carrierHz).toBe(START_HZ + carrierBin * STEP_HZ);
			expect(r.candidate.bin).toBe(carrierBin);
			expect(r.candidate.frames).toBe(3);
			expect(r.candidate.prominenceDb).toBeGreaterThanOrEqual(CARRIER_PROMINENCE_DB);
			expect(r.candidate.binHz).toBe(STEP_HZ);
			expect(r.candidate.lastMs).toBe(120);
		}
	});

});

describe('transient removal', () => {
	it('rejects a one-frame burst that never repeats', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, flatNoise(), 0, ID);
		observeCarrierSpectrum(freq, flatNoise(), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(20, 25), 120, ID); // single burst
		observeCarrierSpectrum(freq, flatNoise(), 180, ID);
		expect(near(START_HZ + 20 * STEP_HZ, 180).ok).toBe(false);
	});

	it('rejects a burst that repeats in only two frames', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, flatNoise(), 0, ID);
		observeCarrierSpectrum(freq, flatNoise(), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(20, 25), 120, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(20, 25), 180, ID);
		expect(near(START_HZ + 20 * STEP_HZ, 180).ok).toBe(false);
	});
});

describe('nearest versus strongest', () => {
	it('picks the nearest eligible carrier, not the strongest distant one', () => {
		const freq = axis();
		const nearBin = 20, farBin = 40;
		const powers = new Float32Array(POINTS).fill(-100);
		powers[farBin] = -40;   // 60 dB over floor: much stronger
		powers[nearBin] = -80;  // 20 dB over floor
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, powers, t * 60, ID);
		const clickHz = START_HZ + nearBin * STEP_HZ;
		const r = near(clickHz, 240);
		expect(r.ok).toBe(true);
		if (r.ok) expect(r.candidate.bin).toBe(nearBin);
	});

	it('refuses comparable rivals near the click as ambiguous', () => {
		const freq = axis();
		const a = 20, b = 24;
		const powers = new Float32Array(POINTS).fill(-100);
		powers[a] = -70;
		powers[b] = -68;
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, powers, t * 60, ID);
		const clickHz = START_HZ + 22 * STEP_HZ; // between them
		const r = near(clickHz, 240);
		expect(r.ok).toBe(false);
		if (!r.ok) expect(r.reason).toBe('ambiguous');
	});
});

describe('staleness and gaps', () => {
	it('reports stale once the newest frame is too old', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 120, ID);
		const r = near(START_HZ + 10 * STEP_HZ, 120 + CARRIER_STALE_MS + 1);
		expect(r.ok).toBe(false);
		if (!r.ok) expect(r.reason).toBe('stale');
	});

	it('decays evidence across a stream gap: the carrier must persist after it', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 60, ID);
		// Gap longer than the staleness window, then one fresh frame only.
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 60 + CARRIER_STALE_MS + 50, ID);
		const r = near(START_HZ + 10 * STEP_HZ, 60 + CARRIER_STALE_MS + 50);
		expect(r.ok).toBe(false);
	});

	it('does not count old frames toward acquisition after their evidence expires', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 370, ID);
		expect(near(START_HZ + 10 * STEP_HZ, 370).ok).toBe(false);
	});
});

describe('geometry and identity', () => {
	it('resets evidence when the capture window moves', () => {
		const freq = axis();
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, frameWithCarrier(10, 20), t * 60, ID);
		const moved = new Float64Array(POINTS);
		for (let i = 0; i < POINTS; i++) moved[i] = START_HZ + 50_000 + i * STEP_HZ;
		observeCarrierSpectrum(moved, frameWithCarrier(10, 20), 240, ID);
		// The old evidence frequency no longer exists on the new axis.
		const r = near(START_HZ + 10 * STEP_HZ, 240, 5000, MAX_BIN_HZ);
		expect(r.ok).toBe(false);
	});

	it('resets evidence when the stream identity changes', () => {
		const freq = axis();
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, frameWithCarrier(10, 20), t * 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 240, ID + 1);
		expect(near(START_HZ + 10 * STEP_HZ, 240).ok).toBe(false);
	});

	it('carries no stale evidence onto a new array with the same axis', () => {
		const freq = axis();
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, frameWithCarrier(10, 20), t * 60, ID);
		// A fresh array object with identical values must keep the history alive.
		const sameValues = axis();
		observeCarrierSpectrum(sameValues, frameWithCarrier(10, 20), 240, ID);
		expect(near(START_HZ + 10 * STEP_HZ, 240).ok).toBe(true);
	});
});

describe('input admission', () => {
	it('rejects a non-uniform native axis honestly', () => {
		const freq = new Float64Array(POINTS);
		for (let i = 0; i < POINTS; i++) freq[i] = START_HZ + i * STEP_HZ * (1 + i / 1e5);
		expect(observeCarrierSpectrum(freq, flatNoise(), 0, ID)).toBe(false);
		expect(near(START_HZ, 0).ok).toBe(false);
	});

	it('rejects non-monotonic and non-finite axes', () => {
		const freq = axis();
		freq[20] = freq[19];
		expect(observeCarrierSpectrum(freq, flatNoise(), 0, ID)).toBe(false);
		const bad = axis();
		bad[20] = NaN;
		expect(observeCarrierSpectrum(bad, flatNoise(), 0, ID)).toBe(false);
	});

	it('excludes gap and edge bins from evidence', () => {
		const freq = axis();
		const powers = new Float32Array(POINTS).fill(-100);
		powers[0] = -70;   // edge bin: can never be judged
		powers[30] = -70;
		powers[31] = NaN;  // gap bin next to a peak: shoulder broken
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, powers, t * 60, ID);
		// The edge bin is not even a stored peak, so a query near it finds nothing.
		const rEdge = near(freq[0], 240, 3000, MAX_BIN_HZ);
		expect(rEdge.ok).toBe(false);
		// The carrier at bin 30 lost its right shoulder: not eligible.
		const r30 = near(START_HZ + 30 * STEP_HZ, 240, 5000, MAX_BIN_HZ);
		expect(r30.ok).toBe(false);
		// Sanity: the same carrier with an intact shoulder is eligible.
		const good = new Float32Array(POINTS).fill(-100);
		good[30] = -70;
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, good, t * 60, ID);
		const rGood = near(START_HZ + 30 * STEP_HZ, 240, 5000, MAX_BIN_HZ);
		expect(rGood.ok).toBe(true);
	});
});

describe('resolution honesty', () => {
	it('refuses a candidate when the native bins are too wide', () => {
		const freq = new Float64Array(POINTS);
		for (let i = 0; i < POINTS; i++) freq[i] = START_HZ + i * 5000;
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(10, 20), 120, ID);
		const r = near(START_HZ + 10 * 5000, 120, 40_000, MAX_BIN_HZ);
		expect(r.ok).toBe(false);
		if (!r.ok) expect(r.reason).toBe('coarse-bins');
		// The same evidence is fine when the caller accepts the wider bin.
		const wide = near(START_HZ + 10 * 5000, 120, 40_000, 5000);
		expect(wide.ok).toBe(true);
	});

	it('reports out-of-view when the search window is off the axis', () => {
		const freq = axis();
		observeCarrierSpectrum(freq, frameWithCarrier(30, 20), 0, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(30, 20), 60, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(30, 20), 120, ID);
		const r = near(500_000, 120, 1000, MAX_BIN_HZ);
		expect(r.ok).toBe(false);
		if (!r.ok) expect(r.reason).toBe('out-of-view');
	});
});


describe('noise-only and peak motion', () => {
	it('never reports a carrier on plain noise', () => {
		const freq = axis();
		// Deterministic pseudo-noise a few dB above the floor, no real peak.
		const powers = new Float32Array(POINTS);
		for (let i = 0; i < POINTS; i++) powers[i] = -100 + 3 * Math.sin(i * 2.399);
		for (let t = 0; t < 4; t++) observeCarrierSpectrum(freq, powers, t * 60, ID);
		for (let i = 1; i < POINTS - 1; i += 7) {
			expect(near(START_HZ + i * STEP_HZ, 240).ok).toBe(false);
		}
	});

	it('follows a carrier whose top moves between adjacent bins', () => {
		const freq = axis();
		for (let t = 0; t < 4; t++) {
			const powers = new Float32Array(POINTS).fill(-100);
			powers[30 + (t % 2)] = -80;
			observeCarrierSpectrum(freq, powers, t * 60, ID);
		}
		const r = near(START_HZ + 30 * STEP_HZ, 240);
		expect(r.ok).toBe(true);
	});
});

describe('ring bounds', () => {
	it('limits stored frames and keeps only recent evidence', () => {
		const freq = axis();
		// Fill well past the ring: a carrier in the earliest frames only.
		for (let t = 0; t < 20; t++) {
			observeCarrierSpectrum(freq, t < 3 ? frameWithCarrier(30, 20) : flatNoise(), t * 60, ID);
		}
		expect(near(START_HZ + 30 * STEP_HZ, 19 * 60).ok).toBe(false);
		// A carrier that stays live for the last frames is still found.
		resetCarrierEvidence();
		for (let t = 0; t < 20; t++) {
			observeCarrierSpectrum(freq, t >= 17 ? frameWithCarrier(30, 20) : flatNoise(), t * 60, ID);
		}
		expect(near(START_HZ + 30 * STEP_HZ, 19 * 60).ok).toBe(true);
	});
});

describe('local credibility across the full view', () => {
	it('rejects raised noise ripples even when far above the global floor', () => {
		const freq = axis();
		const powers = new Float32Array(POINTS).fill(-110);
		for (let i = 15; i <= 45; i++) powers[i] = -65 + 3 * Math.sin(i * 2.399);
		for (const time of [0, 60, 130]) observeCarrierSpectrum(freq, powers, time, ID);
		expect(near(freq[30], 130, 10_000).ok).toBe(false);
	});

	it('finds the true carrier above a raised local floor', () => {
		const freq = axis();
		const powers = new Float32Array(POINTS).fill(-110);
		for (let i = 10; i <= 50; i++) powers[i] = -65 + 2 * Math.sin(i * 2.399);
		powers[30] = -42;
		for (const time of [0, 60, 130]) observeCarrierSpectrum(freq, powers, time, ID);
		const found = near(freq[29], 130, 10_000);
		expect(found.ok && found.candidate.carrierHz).toBe(freq[30]);
	});

	it('accepts asymmetric windowed mainlobes whose second bin has not reached the shoulder', () => {
		const carrierHz = 433_910_120.2755771;
		const freq = Float64Array.from({ length: 128 }, (_, i) => carrierHz + (i - 32) * 240.551);
		const powers = new Float32Array(128).fill(-132);
		const measured = [-148.40434, -141.00809, -127.70866, -123.42566, -123.20860,
			-123.25485, -107.89260, -102.28609, -101.39714, -101.48565, -103.99358,
			-113.34188, -124.95474, -122.60719, -124.01071, -129.61230, -138.20587];
		powers.set(measured, 24);
		powers[95] = -85;
		for (const time of [0, 60, 130]) observeCarrierSpectrum(freq, powers, time, ID);
		const found = near(433_910_000, 130, 20_000, 250);
		expect(found.ok && found.candidate.carrierHz).toBe(carrierHz);
	});

	it('discovers a carrier beyond the first 64 peaks', () => {
		const freq = Float64Array.from({ length: 1024 }, (_, i) => 410_880_000 + i * 240);
		const powers = new Float32Array(1024).fill(-100);
		for (let i = 10; i < 800; i += 10) powers[i] = -80;
		powers[900] = -65;
		for (const time of [0, 60, 130]) observeCarrierSpectrum(freq, powers, time, ID);
		const found = near(freq[900] - 300, 130, 2000, 250);
		expect(found.ok && found.candidate.carrierHz).toBe(freq[900]);
	});

	it('does not accumulate nearby but unrelated noise spikes', () => {
		const freq = axis();
		for (const [bin, time] of [[28, 0], [32, 60], [30, 130]]) {
			observeCarrierSpectrum(freq, frameWithCarrier(bin, 20), time, ID);
		}
		expect(near(freq[30], 130).ok).toBe(false);
	});

	it('validates the acquired native bin across adjacent jitter, but rejects disappearance and distant replacement', () => {
		const freq = axis();
		for (const time of [0, 60, 130]) observeCarrierSpectrum(freq, frameWithCarrier(30, 20), time, ID);
		observeCarrierSpectrum(freq, frameWithCarrier(31, 20), 190, ID);
		expect(currentCarrierEvidence(freq[30], 190, MAX_BIN_HZ)?.hz).toBe(freq[30]);
		observeCarrierSpectrum(freq, frameWithCarrier(35, 40), 250, ID);
		expect(currentCarrierEvidence(freq[30], 250, MAX_BIN_HZ)).toBeNull();
		observeCarrierSpectrum(freq, flatNoise(), 310, ID);
		expect(currentCarrierEvidence(freq[30], 310, MAX_BIN_HZ)).toBeNull();
	});
});
