/**
 * Trace-panel regressions: CSV capture of the live RTA/SDR display, the peak-count
 * editor, limit tolerance reset, Marker All, and waterfall sampling.
 *
 * The RTA CSV rule pinned here: the export takes the ACTIVE display trace (rtaDisplays)
 * but pairs it with rtaData.freq — deliberately stale SWP powers with a frequency grid
 * that does not match rtaData's length are rejected (no half-invented CSV), while a
 * matching-length display/freq pair exports even though the numbers came from different
 * sources. VIEW (frozen) traces export too; an empty active trace T2 refuses with the
 * shared notice and no fallback to another trace.
 *
 * Peak count: the input clamps to 1..20 integers and a 20→2 edit shrinks the table and
 * peakMarks on the same event. Marker All turns every marker on (each on its own peak)
 * and off. Limits Preset zeroes the tolerance input. The waterfall sampler throttles
 * raw sweeps to one row per 100 ms and never samples while paused.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { exportActiveTraceCsv } from '../ui/traceOps';
import { initPeakCountUi, updatePeakTable } from '../render/peaklist';
import { peakListVisible } from '../ui/measurePrefs';
import { resetLimits } from '../ui/limits';
import { toggleMarkersAll, updateMarkersAllBtn } from '../ui/panels/markers';
import {
	resetWf, sampleSwpWaterfall, setWfPaused, setWaterfall,
} from '../ui/panels/waterfall';
import { waterfallOn } from '../ui/waterfallState';

// The peak table reads frequencies through S.freqArray even in RTA mode.
function setGrid(startHz: number, n: number, stepHz: number): Float64Array {
	const freq = new Float64Array(n);
	for (let i = 0; i < n; i++) freq[i] = startHz + i * stepHz;
	S.setFreqArray(freq);
	return freq;
}

/** Peaks at bins 10, 30, 50, 70 (descending amps), noise floor elsewhere. */
function peakyPowers(n: number): Float32Array {
	const p = new Float32Array(n).fill(-100);
	p[10] = -10; p[30] = -20; p[50] = -30; p[70] = -40;
	return p;
}

interface Download { name: string; text: string }

/** Captures CSV blob text and anchor clicks, so exports can be asserted without jsdom downloads. */
let lastBlobText = '';
let downloads: Download[] = [];

function armExports(): void {
	downloads = [];
	lastBlobText = '';
	const OrigBlob = globalThis.Blob;
	vi.stubGlobal('Blob', class extends OrigBlob {
		constructor(parts: BlobPart[], opts?: BlobPropertyBag) {
			super(parts, opts);
			lastBlobText = parts.join('');
		}
	});
	vi.stubGlobal('URL', {
		createObjectURL: () => 'blob:fake',
		revokeObjectURL: () => {},
	} as unknown as typeof URL);
	HTMLAnchorElement.prototype.click = function (this: HTMLAnchorElement) {
		downloads.push({ name: this.download, text: lastBlobText });
	};
}

/** RTA-mode store: live rtaData + the active display trace on T1. */
function rtaFixture(displayPowers: Float32Array, freq: Float64Array) {
	S.setRtaMode(true);
	S.setRtaData({ freq, spec: displayPowers });
	const arr = S.rtaDisplays.slice();
	arr[S.activeTraceIdx] = displayPowers;
	S.setRtaDisplays(arr);
	S.traces[S.activeTraceIdx].mode = 'CLEAR_WRITE';
}

beforeEach(() => {
	document.body.replaceChildren();
	localStorage.clear();
	resetAll();
	S.setRtaMode(false);
	S.setRtaData(null);
	S.setRtaDisplays([null, null, null, null]);
	S.setActiveTraceIdx(0);
	S.traces.forEach((t, i) => { t.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; t.powers = null; });
	S.setPeakMarks(null);
	S.setNoticeText(null);
	S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; m.idx = 0; });
	S.resetWaterfall();
	armExports();
});

afterEach(() => { vi.unstubAllGlobals(); });

describe('exportActiveTraceCsv in RTA mode', () => {
	beforeEach(() => { setGrid(1e9, 100, 1e6); });

	it('pairs the active display powers with rtaData.freq (stale SWP freq ignored)', () => {
		// The display holds a *newer* grid than rtaData: the export must use rtaData.freq,
		// and the mismatched lengths (100 display bins vs 4 rtaData bins) are refused.
		const display = peakyPowers(100);
		rtaFixture(display, new Float64Array([1.1e9, 1.2e9, 1.3e9, 1.4e9]));
		exportActiveTraceCsv();
		expect(downloads).toHaveLength(0);
		expect(S.noticeText).toBe('No data for the active trace');
	});

	it('exports display powers on rtaData.freq when lengths agree', () => {
		const freq = new Float64Array([1.1e9, 1.2e9, 1.3e9, 1.4e9]);
		const display = new Float32Array([-10, -20, -30, -40]);
		rtaFixture(display, freq);
		exportActiveTraceCsv();
		expect(downloads).toHaveLength(1);
		const lines = downloads[0].text.split('\n');
		expect(lines[0]).toBe('# trace=T1');
		expect(lines).toContain('freq_hz,power');
		// span/center derive from rtaData's freq, not from the swept center/span slots.
		expect(lines).toContain('span_hz=300000000');
		expect(lines).toContain('center_hz=1250000000');
		expect(lines[lines.length - 4]).toBe('1100000000.000,-10.000');
		expect(lines[lines.length - 1]).toBe('1400000000.000,-40.000');
	});

	it('exports a frozen (VIEW) display without a fallback notice', () => {
		const freq = new Float64Array([1e9, 2e9]);
		rtaFixture(new Float32Array([-11, -22]), freq);
		S.traces[S.activeTraceIdx].mode = 'VIEW';
		exportActiveTraceCsv();
		expect(downloads).toHaveLength(1);
		expect(S.noticeText).toBeNull();
		expect(downloads[0].text).toContain('mode=VIEW');
	});

	it('an empty active T2 refuses instead of exporting T1 or rtaData.spec', () => {
		const freq = new Float64Array([1e9, 2e9, 3e9]);
		// T1 holds data, T2 (now active) holds none, and rtaData.spec alone cannot satisfy.
		S.setRtaMode(true);
		S.setRtaData({ freq, spec: new Float32Array([-5, -6, -7]) });
		const arr = S.rtaDisplays.slice();
		arr[0] = new Float32Array([-1, -2, -3]);
		S.setRtaDisplays(arr);
		S.setActiveTraceIdx(1);
		S.traces[1].mode = 'CLEAR_WRITE';
		exportActiveTraceCsv();
		expect(downloads).toHaveLength(0);
		expect(S.noticeText).toBe('No data for the active trace');
	});
});

describe('the peak-count editor', () => {
	beforeEach(() => {
		document.body.innerHTML =
			'<input id="input-peakcnt" type="number" value="20">' +
			'<table id="peak-table"><tbody id="peak-tbody"></tbody></table>';
		peakListVisible.set(true);
		setGrid(1e9, 100, 1e6);
		S.traces[0].powers = peakyPowers(100);
		initPeakCountUi();
		updatePeakTable(S.traces[0].powers);
	});

	const cells = () =>
		[...document.querySelectorAll('#peak-tbody .pk-cell')].map(td => td.textContent ?? '');
	const input = () => document.getElementById('input-peakcnt') as HTMLInputElement;

	it('shows all four peaks at the default count 20', () => {
		expect(cells()).toHaveLength(4);
		expect(S.peakMarks).toHaveLength(4);
	});

	it('20→2 shrinks the table and peakMarks on the same input event', () => {
		input().value = '2';
		input().dispatchEvent(new Event('input', { bubbles: true }));
		expect(cells()).toHaveLength(2);
		expect(S.peakMarks!.map(p => p.n)).toEqual([1, 2]);
		expect(S.peakMarks![0].f).toBeCloseTo(1e9 + 10e6, 1);   // bin 10 peak survives
	});

	it('clamps out-of-range and fractional values to 1..20 integers', () => {
		input().value = '99';
		input().dispatchEvent(new Event('input', { bubbles: true }));
		expect(input().value).toBe('20');
		expect(cells()).toHaveLength(4);

		input().value = '0';
		input().dispatchEvent(new Event('input', { bubbles: true }));
		expect(input().value).toBe('1');
		expect(cells()).toHaveLength(1);

		input().value = '2.9';
		input().dispatchEvent(new Event('input', { bubbles: true }));
		expect(input().value).toBe('2');
		expect(cells()).toHaveLength(2);
	});
});

describe('Marker All', () => {
	beforeEach(() => {
		document.body.innerHTML = '<button id="btn-markers-all"></button>';
		setGrid(1e9, 100, 1e6);
		S.traces[0].powers = peakyPowers(100);
	});

	it('turns all four markers on, each on its own strongest-first peak', () => {
		toggleMarkersAll();
		expect(S.markers.every(m => m.enabled && m.mode !== 'OFF')).toBe(true);
		// M1→bin10, M2→bin30, M3→bin50, M4→bin70 (strongest first, no double booking)
		expect(S.markers.map(m => m.idx)).toEqual([10, 30, 50, 70]);
		const btn = document.getElementById('btn-markers-all') as HTMLElement;
		expect(btn.classList.contains('active')).toBe(true);
	});

	it('toggling again turns every marker off', () => {
		toggleMarkersAll();
		toggleMarkersAll();
		expect(S.markers.every(m => !m.enabled && m.mode === 'OFF' && !m.tracking)).toBe(true);
		const btn = document.getElementById('btn-markers-all') as HTMLElement;
		expect(btn.classList.contains('active')).toBe(false);
		updateMarkersAllBtn();
		expect(btn.textContent).toBe('All Off');
	});
});

describe('limits Preset', () => {
	beforeEach(() => {
		document.body.innerHTML =
			'<div id="limit-rows"></div><button id="btn-limit-onoff"></button>' +
			'<input id="input-limit-tol" type="number">';
	});

	it('resets the tolerance input to 0 with the stored state', () => {
		const input = document.getElementById('input-limit-tol') as HTMLInputElement;
		S.setLimits({ on: true, tol: 3.5, points: [{ freqHz: 1e9, level: -30 }, { freqHz: 2e9, level: -30 }] });
		input.value = '3.5';
		resetLimits();
		expect(input.value).toBe('0');
		expect(S.limits.tol).toBe(0);
		expect(S.limits.on).toBe(false);
	});
});

describe('waterfall sweep sampling', () => {
	let now = 0;
	const wfPowers = () => new Float32Array(100).fill(-50);
	const wfFreq = () => {
		const f = new Float64Array(100);
		for (let i = 0; i < 100; i++) f[i] = 1e9 + i * 1e6;
		return f;
	};

	beforeEach(() => {
		// The sampler's last-accept timestamp is module state, so the mocked clock may
		// never run backwards between tests.
		now += 1000;
		vi.spyOn(performance, 'now').mockImplementation(() => now);
		S.setRtaMode(false);
		waterfallOn.set(false);
		setWfPaused(false);
	});

	afterEach(() => { vi.restoreAllMocks(); });

	it('throttles raw sweeps to one row per 100 ms', () => {
		setWaterfall(true);
		const before = S.waterfallPushes;
		sampleSwpWaterfall(wfPowers(), wfFreq());        // first sample: accepted
		expect(S.waterfallPushes).toBe(before + 1);
		now += 50;
		sampleSwpWaterfall(wfPowers(), wfFreq());        // +50 ms: throttled
		expect(S.waterfallPushes).toBe(before + 1);
		now += 60;
		sampleSwpWaterfall(wfPowers(), wfFreq());        // +110 ms: accepted
		expect(S.waterfallPushes).toBe(before + 2);
	});

	it('never samples while paused', () => {
		setWaterfall(true);
		setWfPaused(true);
		const before = S.waterfallPushes;
		now += 500;
		sampleSwpWaterfall(wfPowers(), wfFreq());
		expect(S.waterfallPushes).toBe(before);
	});

	it('Clear empties the sampled history', () => {
		setWaterfall(true);
		sampleSwpWaterfall(wfPowers(), wfFreq());
		expect(S.waterfallRows.length).toBeGreaterThan(0);
		resetWf();
		expect(S.waterfallRows).toHaveLength(0);
		expect(S.waterfallPushes).toBe(0);
	});
});
