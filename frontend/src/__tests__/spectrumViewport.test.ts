/**
 * The display-only spectrum viewport (ui/spectrumViewport.ts).
 *
 * These tests pin the design contract (~/harogic-websa-spectrum-local-zoom-design.md §4):
 * the view is always inside the capture, zoom keeps the anchor under the pointer, a new
 * capture drops the view (a zoom selected against one axis must never be pasted onto
 * another), and none of this touches the wire.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import {
	DETAIL_HINT_SAMPLES,
	MARQUEE_MIN_PX,
	sameWindow,
	type FreqWindow,
	clampView,
	freqToPlotX,
	getCapture,
	getView,
	isValidWindow,
	isZoomed,
	minSpanHz,
	panByFraction,
	panToCenter,
	plotXToFreq,
	resetViewport,
	sampleStepHz,
	samplesInView,
	setCapture,
	setView,
	viewFromMarquee,
	viewportVersion,
	zoomAround,
	zoomEnabled,
	__testSeedCapture,
} from '../ui/spectrumViewport';

const CAP: FreqWindow = { lo: 900e6, hi: 1000e6 };
/** A uniform 1 MHz grid across the capture: 101 points, step 1 MHz. */
const GRID = new Float64Array(101).map((_, i) => 900e6 + i * 1e6);

beforeEach(() => {
	resetAll();
	__testSeedCapture(CAP);
});

afterEach(() => {
	resetViewport();
});

describe('window algebra', () => {
	it('validates windows', () => {
		expect(isValidWindow({ lo: 0, hi: 1 })).toBe(true);
		expect(isValidWindow({ lo: 1, hi: 1 })).toBe(false);
		expect(isValidWindow({ lo: NaN, hi: 2 })).toBe(false);
		expect(isValidWindow(null)).toBe(false);
	});

	it('derives the sample step and the two-step minimum span', () => {
		expect(sampleStepHz(GRID)).toBe(1e6);
		expect(minSpanHz(GRID)).toBe(2e6);
		expect(sampleStepHz(new Float64Array([1]))).toBeNull();
		expect(minSpanHz(null)).toBe(1);   // degenerate: a finite floor, not a pretend grid
	});

	it('clamps a view into the capture and honours the min span', () => {
		// Width is preserved and the window slides inside; a window wider than the capture
		// collapses to the full capture.
		expect(clampView({ lo: 880e6, hi: 905e6 }, CAP, 2e6)).toEqual({ lo: 900e6, hi: 925e6 });
		expect(clampView({ lo: 995e6, hi: 1100e6 }, CAP, 2e6)).toEqual({ lo: 900e6, hi: 1000e6 });
		// Too narrow -> widened to minSpan around the same place.
		const tiny = clampView({ lo: 950e6, hi: 950.001e6 }, CAP, 2e6);
		expect(tiny.hi - tiny.lo).toBeCloseTo(2e6, 3);
	});

	it('zooms around the pointer anchor', () => {
		const anchor = 950e6;
		const v = zoomAround({ lo: 900e6, hi: 1000e6 }, 0.5, anchor, CAP, 2e6);
		expect(v.hi - v.lo).toBeCloseTo(50e6, 3);
		// The anchor keeps its fractional place: it must map to the same fraction again.
		const fracBefore = (anchor - 900e6) / 100e6;
		const fracAfter = (anchor - v.lo) / (v.hi - v.lo);
		expect(fracAfter).toBeCloseTo(fracBefore, 6);
	});

	it('pans by centre and by fraction without changing width', () => {
		const p = panToCenter({ lo: 900e6, hi: 950e6 }, 970e6, CAP, 2e6);
		expect((p.lo + p.hi) / 2).toBeCloseTo(970e6, 3);
		expect(p.hi - p.lo).toBeCloseTo(50e6, 3);
		// A pan past the right edge clamps, it never leaves the capture.
		const edge = panToCenter({ lo: 900e6, hi: 950e6 }, 1200e6, CAP, 2e6);
		expect(edge.hi).toBe(CAP.hi);
		const f = panByFraction({ lo: 900e6, hi: 950e6 }, 0.1, CAP, 2e6);
		expect(f.lo - 900e6).toBeCloseTo(5e6, 3);
	});

	it('builds a view from a marquee in either drag direction', () => {
		const a = viewFromMarquee(940e6, 960e6, CAP, 2e6);
		const b = viewFromMarquee(960e6, 940e6, CAP, 2e6);
		expect(a).toEqual(b);
		expect(a.lo).toBeCloseTo(940e6, 3);
		// A degenerate drag becomes a minimal window around the midpoint, not a rejection.
		const tiny = viewFromMarquee(950e6, 950e6, CAP, 2e6);
		expect(tiny.hi - tiny.lo).toBeCloseTo(2e6, 3);
		expect(tiny.lo >= CAP.lo && tiny.hi <= CAP.hi).toBe(true);
	});

	it('maps frequency and plot x both ways', () => {
		const view: FreqWindow = { lo: 900e6, hi: 1000e6 };
		const rect = { x: 10, w: 800 };
		expect(freqToPlotX(900e6, view, rect)).toBe(10);
		expect(freqToPlotX(1000e6, view, rect)).toBe(810);
		expect(plotXToFreq(410, view, rect)).toBeCloseTo(950e6, 3);
		// Zoomed view: the same rect covers less spectrum.
		const zoomed: FreqWindow = { lo: 950e6, hi: 960e6 };
		expect(plotXToFreq(410, zoomed, rect)).toBeCloseTo(955e6, 3);
	});

	it('counts real samples in the view for the detail hint', () => {
		expect(samplesInView(GRID, { lo: 900e6, hi: 1000e6 })).toBe(101);
		expect(samplesInView(GRID, { lo: 940e6, hi: 950e6 })).toBe(11);
		expect(samplesInView(null, CAP)).toBeNull();
		expect(samplesInView(GRID, { lo: 940e6, hi: 950e6 })! > DETAIL_HINT_SAMPLES).toBe(true);
	});
});

describe('module state', () => {
	it('defaults to the full capture and no zoom', () => {
		expect(sameWindow(getCapture(), CAP)).toBe(true);
		expect(sameWindow(getView(), CAP)).toBe(true);
		expect(isZoomed()).toBe(false);
	});

	it('keeps the view across frames of the same capture', () => {
		setView({ lo: 940e6, hi: 960e6 }, 2e6);
		expect(isZoomed()).toBe(true);
		// The renderer feeds the same window every frame (float jitter included).
		expect(setCapture({ lo: 900e6 + 0.1, hi: 1000e6 - 0.1 })).toBe(false);
		expect(isZoomed()).toBe(true);
	});

	it('drops the view when the capture actually changes', () => {
		setView({ lo: 940e6, hi: 960e6 }, 2e6);
		expect(setCapture({ lo: 800e6, hi: 900e6 })).toBe(true);
		expect(isZoomed()).toBe(false);
		expect(sameWindow(getView(), { lo: 800e6, hi: 900e6 })).toBe(true);
	});

	it('clears everything on invalid capture and on reset', () => {
		setView({ lo: 940e6, hi: 960e6 }, 2e6);
		expect(setCapture(null)).toBe(true);
		expect(getCapture()).toBeNull();
		expect(getView()).toBeNull();
		__testSeedCapture(CAP);
		setView({ lo: 940e6, hi: 960e6 }, 2e6);
		resetViewport();
		expect(getCapture()).toBeNull();
	});

	it('bumps the version only on real changes', () => {
		const v0 = viewportVersion();
		expect(setView({ lo: 940e6, hi: 960e6 }, 2e6)).toBe(true);
		const v1 = viewportVersion();
		expect(v1).toBeGreaterThan(v0);
		expect(setView({ lo: 940e6 + 0.1, hi: 960e6 - 0.1 }, 2e6)).toBe(false);   // same window
		expect(viewportVersion()).toBe(v1);
	});

	it('keeps the zoom toggle in memory only, Off by default', () => {
		expect(zoomEnabled.get()).toBe(false);
		zoomEnabled.set(true);
		expect(zoomEnabled.get()).toBe(true);
	});
});

describe('gesture constants', () => {
	it('matches the design thresholds', () => {
		expect(MARQUEE_MIN_PX).toBe(8);
		expect(DETAIL_HINT_SAMPLES).toBe(8);
	});
});
