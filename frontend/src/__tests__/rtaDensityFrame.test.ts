/** Exercise the real RTAF receiver: density must follow the selected processed trace. */
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { connectWS } from '../core/ws';
import { clearDisplayBounds, getDisplayRange, getDisplayTop, setDisplayBounds, setDisplayRef } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';
import { rtaAmpBins, rtaDensSource, rtaHalflife, waterfallOn } from '../ui/waterfallState';
import { setRtaDensSource, beginRtaAxisTransition, clearRtaAccum, rejectRtaAxisTransition, resetRtaAxisTransition } from '../ui/panels/rta';
import { clearRtaTrace, setTraceMode, switchTraceTab } from '../ui/traceOps';
import { zoomEnabled } from '../ui/spectrumViewport';
import { densityLevelPlacement, densityLut, paintDensity, rasterDensity } from '../dsp/rtaDensity';

class DisplaySocket {
	static OPEN = 1;
	static CONNECTING = 0;
	readyState = 1;
	binaryType = '';
	onmessage: ((event: MessageEvent) => void) | null = null;
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	send() {}
}
let socket: DisplaySocket;
let clock = 1000;

/** Minimal valid native RTAF with an optional changed frequency grid. */
function feed(peakDb: number, shift = 0, version = 1) {
	const points = 32;
	const data = new ArrayBuffer(24 + points * 12 + 2 + 8);
	new Uint8Array(data, 0, 4).set([82, 84, 65, 70]);
	const hdr = new DataView(data);
	hdr.setUint32(4, version, true);
	hdr.setUint32(8, points, true);
	hdr.setUint16(12, 1, true);
	hdr.setFloat64(16, 1e9, true);
	const freq = new Float64Array(data, 24, points);
	const spec = new Float32Array(data, 24 + points * 8, points);
	for (let i = 0; i < points; i++) freq[i] = 1e9 + i * 100 + shift;
	spec.fill(-120);
	spec[16] = peakDb;
	hdr.setFloat64(data.byteLength - 8, 1e9 + 3200, true);
	socket.onmessage!({ data } as MessageEvent);
}

beforeAll(() => {
	vi.stubGlobal('WebSocket', class extends DisplaySocket {
		constructor() { super(); socket = this; }
	});
	vi.spyOn(performance, 'now').mockImplementation(() => (clock += 100));
	connectWS();
});
afterAll(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});
beforeEach(() => {
	document.body.replaceChildren();
	resetRtaAxisTransition();
	S.setActiveTraceIdx(0);
	S.setRtaData(null);
	S.setRtaDisplays([null, null, null, null]);
	S.setRtaDensity2d(null);
	S.traces.forEach((tr, i) => { tr.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; });
	S.traces[0].avgTargetRta = 2;
	clearRtaTrace();
	setDisplayRef('preset', 0);
	S.setDbPerDiv(10);
	displayOffset.set(0);
	rtaAmpBins.set(128);
	// Abandoned hits vanish on the second frame, exposing which data the receiver adds.
	rtaHalflife.set(0.001);
	waterfallOn.set(false);
});

describe.each([false, true])('processed active-trace density (SDR=%s)', (sdr) => {
	beforeEach(() => { S.setSdrMode(sdr); rtaDensSource.set('trace'); });
	it('reprojects retained SDR occupancy when display bounds move', () => {
		S.setSdrMode(true);
		zoomEnabled.set(true);
		try {
			rtaHalflife.set(2);
			feed(-40.625);
			const grid = S.rtaDensity2d!;
			const freq = S.rtaData.freq;
			const raster = new Float32Array(100 * grid.bins);
			rasterDensity(grid, freq, freq[16], freq[17], 100, raster);
			const paint = () => {
				const placement = densityLevelPlacement(grid, getDisplayTop(), getDisplayRange(), displayOffset.get());
				const pitch = placement.height * 100 / grid.bins;
				const pixels = new Uint8ClampedArray(100 * 100 * 4);
				paintDensity(raster, 100, grid.bins, pixels, 100,
					placement.top * 100 + pitch / 2, pitch, 1, 1, densityLut(false));
				return pixels;
			};
			expect(paint()[(40 * 100) * 4 + 3]).toBeGreaterThan(0);
			const before = grid.values[16 * grid.bins + 52];
			setDisplayBounds(-35, -45);
			expect(S.rtaDensity2d).toBe(grid);
			expect(grid.values[16 * grid.bins + 52]).toBe(before);
			const moved = paint();
			expect(moved[(56 * 100) * 4 + 3]).toBeGreaterThan(0);
			expect(moved[(40 * 100) * 4 + 3]).toBe(0);
			feed(-40.625);
			expect(S.rtaDensity2d).toBe(grid);
			expect(grid.values[16 * grid.bins + 52]).toBeGreaterThan(before);
		} finally {
			clearDisplayBounds();
			zoomEnabled.set(false);
			S.setSdrMode(sdr);
		}
	});
	it('accumulates steady packets into the same native occupancy history', () => {
		rtaHalflife.set(2);
		feed(-40.625);
		const grid = S.rtaDensity2d!;
		const row = Math.round((grid.refTop + 40.625) / grid.dBPerBin);
		const before = grid.values[16 * grid.bins + row];
		feed(-40.625);
		expect(S.rtaDensity2d).toBe(grid);
		expect(grid.values[16 * grid.bins + row]).toBeGreaterThan(before);
	});
	it('halves abandoned occupancy after one wall-clock half-life', () => {
		rtaHalflife.set(1);
		const timed = vi.spyOn(performance, 'now').mockReturnValue(clock + 1000);
		try {
			feed(-40.625);
			const grid = S.rtaDensity2d!;
			const row = Math.round((grid.refTop + 40.625) / grid.dBPerBin);
			const before = grid.values[16 * grid.bins + row];
			timed.mockReturnValue(clock + 2000);
			feed(-80);
			expect(S.rtaDensity2d).toBe(grid);
			expect(grid.values[16 * grid.bins + row]).toBeCloseTo(before / 2, 5);
		} finally {
			timed.mockImplementation(() => (clock += 100));
		}
	});
	it.each([
		['AVERAGE', -40 + (-80 + 40) * 2 / 3],
		['MAX_HOLD', -40],
		['MIN_HOLD', -80],
	])('uses %s output while retaining the raw frame for waterfall', (mode, expected) => {
		setTraceMode(mode as string);
		feed(-40);
		feed(-80);
		const displayed = S.rtaDisplays[0]![16];
		expect(displayed).toBeCloseTo(expected as number, 4);
		expect(S.rtaData.spec[16]).toBe(-80);
		const grid = S.rtaDensity2d!;
		const row = Math.round((grid.refTop - displayed) / grid.dBPerBin);
		expect(grid.values[16 * grid.bins + row]).toBeGreaterThan(0);
		const rawRow = Math.round((grid.refTop + 80) / grid.dBPerBin);
		if (Math.abs(rawRow - row) > 2) expect(grid.values[16 * grid.bins + rawRow]).toBe(0);
	});

	it('uses the selected trace and freezes its history when VIEW receives new packets', () => {
		setTraceMode('MAX_HOLD');
		S.traces[1].mode = 'MIN_HOLD';
		feed(-40); feed(-80);
		switchTraceTab(1);
		feed(-50);
		expect(S.rtaDisplays[1]![16]).toBe(-80);
		const grid = S.rtaDensity2d!;
		const row = Math.round((grid.refTop + 80) / grid.dBPerBin);
		expect(grid.values[16 * grid.bins + row]).toBeGreaterThan(0);
		setTraceMode('VIEW');
		const before = grid.values.slice();
		feed(-20);
		expect(S.rtaDisplays[1]![16]).toBe(-80);
		expect(S.rtaDensity2d).toBe(grid);
		expect(grid.values).toEqual(before);
	});

	it('invalidates history on same-length native grid changes and reanchors new amplitude geometry', () => {
		feed(-40);
		const old = S.rtaDensity2d!;
		feed(-80, 1);
		expect(S.rtaDensity2d).not.toBe(old);
		displayOffset.set(9);
		setDisplayRef('preset', -20);
		S.setDbPerDiv(5);
		feed(-60, 1);
		const moved = S.rtaDensity2d!;
		expect(moved.refTop).toBe(-29);
		expect(moved.dBPerBin).toBe(S.totalDivs * 5 / 128);
		const row = Math.round((-29 + 60) / moved.dBPerBin);
		expect(moved.values[16 * moved.bins + row]).toBeGreaterThan(0);
	});
	it('does not rebuild cleared history from old retune packets, and releases a rejected retune', () => {
		feed(-40);
		beginRtaAxisTransition('retune-A');
		clearRtaAccum();
		feed(-80);
		expect(S.rtaDensity2d).toBeNull();
		expect(S.rtaDisplays[0]).toBeNull();
		feed(-80, 10000, 2);
		expect(S.rtaDisplays[0]![16]).toBe(-80);
		expect(S.rtaDensity2d!.freq).toBe(S.rtaData.freq);
		beginRtaAxisTransition('retune-B');
		clearRtaAccum();
		rejectRtaAxisTransition('SET_RTA', 'retune-B');
		feed(-60, 10000, 2);
		expect(S.rtaDisplays[0]![16]).toBe(-60);
	});
	it('keeps the latest retune gated when an older or uncorrelated request fails', () => {
		feed(-40);
		beginRtaAxisTransition('retune-A');
		beginRtaAxisTransition('retune-B');
		clearRtaAccum();
		rejectRtaAxisTransition('SET_RTA', 'retune-A');
		feed(-80);
		expect(S.rtaDensity2d).toBeNull();
		expect(S.rtaDisplays[0]).toBeNull();
		rejectRtaAxisTransition('SET_RTA');
		feed(-80);
		expect(S.rtaDensity2d).toBeNull();
		expect(S.rtaDisplays[0]).toBeNull();
		rejectRtaAxisTransition('SET_RTA', 'retune-B');
		feed(-60);
		expect(S.rtaDisplays[0]![16]).toBe(-60);
		expect(S.rtaDensity2d!.freq).toBe(S.rtaData.freq);
	});
	it('replaces both layers on the first new-axis packet inside the process throttle', () => {
		rtaHalflife.set(999);
		const rapid = vi.spyOn(performance, 'now').mockReturnValue(clock + 1000);
		feed(-40);
		const old = S.rtaDensity2d!;
		rapid.mockReturnValue(clock + 1001);
		try {
			feed(-80, 10000, 2);
			const grid = S.rtaDensity2d!;
			expect(grid).not.toBe(old);
			expect(grid.freq).toBe(S.rtaData.freq);
			expect(S.rtaData.ver).toBe(2);
			expect(S.rtaDisplays[0]![16]).toBe(-80);
			const oldRow = Math.round((grid.refTop + 40) / grid.dBPerBin);
			expect(grid.values[16 * grid.bins + oldRow]).toBe(0);
			feed(-20, 0, 1);
			expect(S.rtaDensity2d).toBe(grid);
			expect(S.rtaDisplays[0]![16]).toBe(-80);
		} finally {
			rapid.mockImplementation(() => (clock += 100));
		}
	});

	it('live source accumulates the raw frame and survives trace freeze', () => {
		rtaDensSource.set('live');
		feed(-40);
		const raw = S.rtaData.spec[16];
		const grid = S.rtaDensity2d!;
		expect(grid.source).toBe('live');
		const row = Math.round((grid.refTop - raw) / grid.dBPerBin);
		expect(grid.values[16 * grid.bins + row]).toBeGreaterThan(0);
		setTraceMode('VIEW');
		feed(-80);
		expect(grid.revision).toBeGreaterThan(1);   // VIEW freezes the trace, not this history
		expect(grid.values[16 * grid.bins + Math.round((grid.refTop + 80) / grid.dBPerBin)]).toBeGreaterThan(0);
	});
	it('switching the source back starts a fresh trace-following history', () => {
		setRtaDensSource('live');
		feed(-40);
		const liveGrid = S.rtaDensity2d!;
		setRtaDensSource('trace');
		expect(S.rtaDensity2d).toBeNull();
		feed(-40);
		expect(S.rtaDensity2d).not.toBe(liveGrid);
		expect(S.rtaDensity2d!.source).toBe('trace');
	});
});
