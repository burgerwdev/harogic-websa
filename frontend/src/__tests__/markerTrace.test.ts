import { beforeEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { accumulateTrace, applyMode } from '../dsp/accumulator';
import { getDisplayPowers } from '../dsp/peaks';
import { rankedSignalPeaks } from '../dsp/markerTracking';
import { renderAll } from '../render/spectrum';
import { peakListVisible, peakThr, peakThrUserSet } from '../ui/measurePrefs';
import { smoothBins } from '../ui/displayState';
import { activeMarkerPeak } from '../ui/panels/markers';
import { toggleMarkerTracking, updateTrackingMarkers } from '../dsp/markerTracking';
import { findPeaks } from '../render/peaklist';

const FREQ = Float64Array.from({ length: 201 }, (_, i) => 100e6 + i * 1000);
function signal(peaks: Record<number, number>): Float32Array {
	const powers = new Float32Array(FREQ.length).fill(-100);
	for (const [index, level] of Object.entries(peaks)) powers[Number(index)] = level;
	return powers;
}

beforeEach(() => {
	resetAll();
	document.body.innerHTML = '<input id="input-peakcnt" value="20">' +
		'<table id="marker-table"></table><table id="peak-table"><tbody id="peak-tbody"></tbody></table>';
	S.setFreqArray(FREQ);
	S.setActiveTraceIdx(0);
	S.setRtaMode(false);
	S.setSdrMode(false);
	S.setViewMode('std');
	S.setRtaData(null);
	S.setRtaDisplays([null, null, null, null]);
	S.setMeasOn(false);
	S.setPeakMarks(null);
	S.traces.forEach((trace, index) => Object.assign(trace, {
		mode: index === 0 ? 'CLEAR_WRITE' : 'OFF', powers: null, raw: null,
		avgSum: null, avgCount: 0, avgTarget: 4, done: false, reference: null, isNormalized: false,
	}));
	S.markers.forEach(marker => Object.assign(marker, { enabled: false, mode: 'OFF', tracking: false, freq: null }));
	S.setActiveMkrId(1);
	peakListVisible.set(true);
	peakThr.set(-80);
	peakThrUserSet.set(true);
	smoothBins.set(1);
});

describe('strongest peak selection', () => {
	it('includes a newly held stronger peak beyond many earlier weak peaks', () => {
		const powers = signal({ 180: -10 });
		for (let i = 0; i < 19; i++) powers[5 + i * 8] = -60;
		const peaks = findPeaks(powers, 2);
		expect(peaks.map(peak => peak.idx)).toEqual([180, 5]);
	});

	it('keeps Tracking armed through missing signals and follows a newly appearing peak', () => {
		const marker = S.markers[0];
		toggleMarkerTracking(marker);
		expect(marker.tracking).toBe(true);
		S.traces[0].powers = signal({ 30: -20 });
		updateTrackingMarkers();
		expect(marker.idx).toBe(30);
		S.traces[0].powers = signal({ 30: -20, 150: -10 });
		updateTrackingMarkers();
		expect(marker.idx).toBe(150);
		S.traces[0].powers = signal({});
		updateTrackingMarkers();
		expect(marker.tracking).toBe(true);
		S.traces[0].powers = signal({ 90: -10 });
		updateTrackingMarkers();
		expect(marker.idx).toBe(90);
		toggleMarkerTracking(marker);
		expect(marker.tracking).toBe(false);
		const manualIndex = marker.idx;
		S.traces[0].powers = signal({ 90: -5 });
		updateTrackingMarkers();
		expect(marker.idx).toBe(manualIndex);
	});

	it('keeps Peak as a single search while Tracking is off', () => {
		S.traces[0].powers = signal({ 30: -20 });
		activeMarkerPeak();
		const marker = S.markers[0];
		expect(marker.idx).toBe(30);
		expect(marker.tracking).toBe(false);
		S.traces[0].powers = signal({ 150: -10 });
		updateTrackingMarkers();
		expect(marker.idx).toBe(30);
	});
});

const MODES = [
	{ mode: 'CLEAR_WRITE', expected: { 30: -30, 90: -10 } },
	{ mode: 'MAX_HOLD', expected: { 30: -20, 90: -10, 150: -40 } },
	{ mode: 'MIN_HOLD', expected: { 30: -30 } },
	{ mode: 'AVERAGE', expected: { 30: -24, 90: -64, 150: -64 } },
	{ mode: 'VIEW', expected: { 30: -20, 150: -40 } },
];

describe.each(['swp', 'rta', 'sdr'])('marker and peak list source in %s', family => {
	function show(processed: Float32Array | null, live: Float32Array): void {
		S.setRtaMode(family !== 'swp');
		S.setSdrMode(family === 'sdr');
		S.setViewMode(family === 'swp' ? 'std' : 'rta');
		S.setRtaData({ freq: FREQ, spec: live, startHz: FREQ[0], stopHz: FREQ[FREQ.length - 1] });
		if (family !== 'swp') {
			S.setRtaDisplays([processed, null, null, null]);
			// A previous swept trace must not become the real-time measurement source.
			S.traces[0].powers = signal({ 180: -5 });
		} else S.traces[0].powers = processed;
	}

	it.each(MODES)('uses the $mode trace for peak frequencies and levels', ({ mode, expected }) => {
		const trace = S.traces[0];
		applyMode(trace, mode);
		accumulateTrace(trace, signal({ 30: -20, 150: -40 }));
		const live = signal({ 30: -30, 90: -10 });
		accumulateTrace(trace, live);
		show(trace.powers, live);
		renderAll();
		const peaks = S.peakMarks!;
		const ranked = Object.entries(expected).sort((a, b) => b[1]! - a[1]! || Number(a[0]) - Number(b[0]));
		expect(peaks.map(peak => peak.idx)).toEqual(ranked.map(([index]) => Number(index)));
		for (const peak of peaks) {
			expect(peak.amp).toBeCloseTo(expected[peak.idx as keyof typeof expected]!, 4);
			expect(peak.f).toBe(FREQ[peak.idx]);
		}
		expect(rankedSignalPeaks().map(peak => peak.idx).sort((a, b) => a - b))
			.toEqual(Object.keys(expected).map(Number));
		const marker = S.markers[0];
		Object.assign(marker, { enabled: true, mode: 'NORMAL', tracking: true, idx: 30, freq: FREQ[30] });
		updateTrackingMarkers();
		expect(marker.idx).toBe(Number(ranked[0][0]));
	});

	it('does not substitute live data or another trace for an empty selected trace', () => {
		show(null, signal({ 90: -10 }));
		S.traces[1].mode = 'MAX_HOLD';
		S.traces[1].powers = signal({ 150: -20 });
		expect(getDisplayPowers()).toBeNull();
		expect(rankedSignalPeaks()).toEqual([]);
		renderAll();
		expect(S.peakMarks).toEqual([]);
	});

	it('does not report retained samples from a disabled selected trace', () => {
		show(signal({ 30: -20 }), signal({ 90: -10 }));
		S.traces[0].mode = 'OFF';
		expect(getDisplayPowers()).toBeNull();
		expect(rankedSignalPeaks()).toEqual([]);
		renderAll();
		expect(S.peakMarks).toEqual([]);
	});

	it('restores the marker table when PK List is closed while no trace data is available', () => {
		show(null, signal({}));
		renderAll();
		expect(document.getElementById('peak-table')!.style.display).toBe('');
		peakListVisible.set(false);
		renderAll();
		expect(document.getElementById('peak-table')!.style.display).toBe('none');
		expect(document.getElementById('marker-table')!.style.display).toBe('');
	});
});
