/**
 * Gesture math for the display-only spectrum zoom.
 *
 * Split out of ui/controls.ts so the decision logic is unit-testable without a canvas:
 * everything here is pure state + the viewport algebra, and the module provably has no
 * wire access (the static test asserts no wsSend import / send() call — design §6: zoom
 * operations must never produce SET_FREQ/SET_RTA/SET_SDR traffic).
 *
 * Main plot (only while the top-bar toggle is On):
 *   - press inside the plot: a CLICK keeps the original semantics (marker / SDR tune /
 *     Shift+click), a drag past MARQUEE_MIN_PX becomes a marquee that owns the gesture;
 *   - wheel inside the plot zooms around the pointer, unless the browser-zoom modifiers
 *     (Ctrl/Cmd) are held;
 *   - the axis-label bands keep their DEVICE meaning (ui/axisDrag.ts), so a device gesture
 *     first drops the local view: a capture preview and a display zoom must never stack.
 *
 * Overview box: see ui/spectrumZoomUi.ts (DOM/pointer), which uses freqAtFraction below.
 */
import { MARGIN, W } from '../core/store';
import {
	MARQUEE_MIN_PX,
	type FreqWindow,
	getCapture,
	getView,
	setView,
	viewFromMarquee,
	zoomAround,
} from './spectrumViewport';

/** The in-progress marquee the renderer may draw as a preview rectangle. */
let marqueePreview: FreqWindow | null = null;

export function getMarqueePreview(): FreqWindow | null {
	return marqueePreview;
}

export function cancelMarqueePreview(): boolean {
	if (marqueePreview === null) return false;
	marqueePreview = null;
	return true;
}

/**
 * Frequency under a canvas x using the window the plot is CURRENTLY drawn in
 * (the view when zoomed, else the capture). Null outside the plot rectangle.
 *
 * This is the single inverse mapping every plot-side input must use once the renderer
 * draws the view: marker clicks, SDR tune clicks and the marquee endpoints all read it,
 * so pixels and data can never disagree.
 */
export function spectrumFreqAtX(x: number): number | null {
	const view = getView();
	if (!view) return null;
	// The plot rectangle from the store primitives directly (the same arithmetic as
	// render/plot.plotRect) — importing plot.ts here would drag the renderer graph into
	// every ui/ importer of this module.
	const p = { x: MARGIN.left, w: W - MARGIN.left - MARGIN.right };
	if (x < p.x || x > p.x + p.w) return null;
	return view.lo + (x - p.x) / p.w * (view.hi - view.lo);
}

/** True once a press has moved far enough horizontally to be a marquee, not a click. */
export function isMarqueeDrag(dxPx: number, dyPx: number): boolean {
	return Math.abs(dxPx) >= MARQUEE_MIN_PX && Math.abs(dxPx) >= Math.abs(dyPx);
}

/** Live marquee preview between two canvas x positions (vertical motion ignored). */
export function updateMarqueePreview(x0: number, x1: number): boolean {
	const a = spectrumFreqAtX(Math.min(x0, x1));
	const b = spectrumFreqAtX(Math.max(x0, x1));
	if (a === null || b === null) return cancelMarqueePreview();
	const next = { lo: Math.min(a, b), hi: Math.max(a, b) };
	const changed = marqueePreview === null
		|| Math.abs(marqueePreview.lo - next.lo) > 1
		|| Math.abs(marqueePreview.hi - next.hi) > 1;
	marqueePreview = next;
	return changed;
}

/**
 * Finish a marquee: commit the previewed window as the view. Returns true when the view
 * changed. A degenerate (sub-min-span) selection widens to the minimum around the midpoint
 * (viewFromMarquee), it never refuses.
 */
export function commitMarquee(x0: number, x1: number, minSpan: number): boolean {
	marqueePreview = null;
	if (!isSpectrumZoomable()) return false;
	const view = getView();
	const capture = getCapture();
	if (!view || !capture) return false;
	const next = viewFromMarquee(
		spectrumFreqAtX(Math.min(x0, x1)) ?? view.lo,
		spectrumFreqAtX(Math.max(x0, x1)) ?? view.hi,
		capture, minSpan);
	const before = view;
	setView(next, minSpan);
	return !sameWindowLoose(before, getView());
}

/** Wheel zoom on the main plot: factor 0.8 / 1.25 per notch, anchored at the pointer. */
export function spectrumWheelZoom(deltaY: number, x: number, minSpan: number): boolean {
	if (!isSpectrumZoomable()) return false;
	const view = getView();
	const capture = getCapture();
	if (!view || !capture) return false;
	const f = spectrumFreqAtX(x);
	if (f === null) return false;                    // outside the plot: not our gesture
	const factor = deltaY < 0 ? 0.8 : 1.25;
	const next = zoomAround(view, factor, f, capture, minSpan);
	setView(next, minSpan);
	return true;
}

/**
 * A device-side capture gesture (axis band drag/wheel) starts: the local view must go
 * back to the full window FIRST (design §3) so the axisDrag preview transform and a
 * display zoom never stack on the same frame.
 */
export function resetViewForDeviceGesture(): boolean {
	const had = getView() !== null && isViewSubwindow();
	setView(null);
	return had;
}

/** Zoom gestures only exist while the toggle is On and a frame has been drawn. */
function isSpectrumZoomable(): boolean {
	// Imported lazily via the viewport module's own state; zoomEnabled read through it
	// would drag the param in everywhere, so the toggle check stays with the caller
	// (controls.ts) which already imports isSpectrumZoomOn. Here: data readiness only.
	return getCapture() !== null;
}

function isViewSubwindow(): boolean {
	const v = getView(), c = getCapture();
	return !!v && !!c && (Math.abs(v.lo - c.lo) > 0.5 || Math.abs(v.hi - c.hi) > 0.5);
}

function sameWindowLoose(a: FreqWindow | null, b: FreqWindow | null): boolean {
	return !!a && !!b && Math.abs(a.lo - b.lo) <= 0.5 && Math.abs(a.hi - b.hi) <= 0.5;
}
