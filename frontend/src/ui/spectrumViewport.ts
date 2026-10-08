/**
 * Display-only spectrum viewport: a Hz window the main plot shows inside the window the
 * device actually captured.
 *
 * This is the "navigator" of the local-zoom design (~/harogic-websa-spectrum-local-zoom-design.md):
 * the user keeps the acquisition (Center/Span/RBW/VBW/Points) untouched and magnifies the
 * ALREADY captured spectrum. Everything in this module is client-side display state:
 *
 *   - it never imports wsSend / send, never issues a command, never touches centerHz/spanHz;
 *   - `capture` is derived from the frame the renderer is drawing (S.freqArray for SWP,
 *     rtaData.startHz/stopHz for RTA/SDR) — never from the editor inputs;
 *   - `view` is always inside `capture` (C0 <= lo < hi <= C1), clamped to a minimum span of
 *     two sample steps so a magnified trace still shows real samples;
 *   - nothing persists: a reload, a mode switch, a Preset or a capture-window change returns
 *     to the full window (design §4.3).
 *
 * The functions are pure and the state is tiny on purpose: the gesture layer (ui/controls.ts)
 * computes the next window here and then calls requestRender() itself, so this file stays a
 * leaf that unit tests can drive without a canvas or a WebSocket.
 */
import { createParam } from '../core/params';

export type FreqWindow = { lo: number; hi: number };

/**
 * Master zoom toggle (the top-bar button next to the keypad). A client-owned preference that
 * is deliberately NOT persisted (design §3: Off after a reload), so `authoritative` with no
 * persistKey: `get()` is whatever the user last clicked, `resetAll('zoom')` clears it.
 */
export const zoomEnabled = createParam<boolean>('zoom.enabled', {
	fallback: false, scope: 'zoom', authoritative: true,
});

/** Two windows closer than this are the same window (per-frame float jitter must not reset). */
export const SAME_WINDOW_HZ = 0.5;
/** The marquee only becomes a zoom past this width (CSS px; the gesture layer measures it). */
export const MARQUEE_MIN_PX = 8;
/** Below this many samples in the view the UI shows the "no new detail" hint (design §3). */
export const DETAIL_HINT_SAMPLES = 8;

// ---------------- window algebra (pure) ----------------

/** A window is usable when both ends are finite and ordered. */
export function isValidWindow(w: FreqWindow | null | undefined): w is FreqWindow {
	return !!w && Number.isFinite(w.lo) && Number.isFinite(w.hi) && w.hi > w.lo;
}

/**
 * Median positive step of an (increasing) frequency array: the sample spacing the min-span
 * rule is derived from. The median ignores the SDR rebin gaps at the edges; null when the
 * array is too small/degenerate.
 */
export function sampleStepHz(freq: ArrayLike<number> | null | undefined): number | null {
	if (!freq || freq.length < 2) return null;
	const steps: number[] = [];
	for (let i = 1; i < freq.length; i++) {
		const d = freq[i] - freq[i - 1];
		if (Number.isFinite(d) && d > 0) steps.push(d);
	}
	if (!steps.length) return null;
	steps.sort((a, b) => a - b);
	return steps[steps.length >> 1];
}

/** Minimum view width: two sample steps (≥ 3 real samples), so zoom always shows real data. */
export function minSpanHz(freq: ArrayLike<number> | null | undefined): number {
	const step = sampleStepHz(freq);
	// A degenerate grid still needs SOME floor: 1 Hz keeps the algebra finite without
	// pretending there is detail (the sample-count hint covers the UX side).
	return step ? step * 2 : 1;
}

/** Same window (within SAME_WINDOW_HZ on both ends). */
export function sameWindow(a: FreqWindow | null, b: FreqWindow | null): boolean {
	return !!a && !!b
		&& Math.abs(a.lo - b.lo) <= SAME_WINDOW_HZ
		&& Math.abs(a.hi - b.hi) <= SAME_WINDOW_HZ;
}

/** Clamp [lo,hi] into capture honouring minSpan: shrink at the edges, never exceed capture. */
export function clampView(w: FreqWindow, capture: FreqWindow, minSpan: number): FreqWindow {
	const cap = Math.max(capture.hi - capture.lo, minSpan);   // degenerate capture: full width
	const span = Math.min(Math.max(w.hi - w.lo, minSpan), cap);
	let lo = Math.min(Math.max(w.lo, capture.lo), capture.hi - span);
	let hi = lo + span;
	// Rounding guards: keep exactly inside the capture on both ends.
	if (hi > capture.hi) { hi = capture.hi; lo = hi - span; }
	return { lo, hi };
}

/** Zoom by `factor` (<1 = zoom in, >1 = out) keeping `anchorHz` fixed under the pointer. */
export function zoomAround(view: FreqWindow, factor: number, anchorHz: number,
	capture: FreqWindow, minSpan: number): FreqWindow {
	const span = (view.hi - view.lo) * factor;
	// Keep the anchor at its fractional place inside the new window.
	const frac = (anchorHz - view.lo) / (view.hi - view.lo);
	const lo = anchorHz - frac * span;
	return clampView({ lo, hi: lo + span }, capture, minSpan);
}

/** Pan so the window centre lands on `centerHz` (width unchanged). */
export function panToCenter(view: FreqWindow, centerHz: number,
	capture: FreqWindow, minSpan: number): FreqWindow {
	const span = view.hi - view.lo;
	return clampView({ lo: centerHz - span / 2, hi: centerHz + span / 2 }, capture, minSpan);
}

/** Pan by a fraction of the view width (+1 = one full width right). */
export function panByFraction(view: FreqWindow, frac: number,
	capture: FreqWindow, minSpan: number): FreqWindow {
	const d = frac * (view.hi - view.lo);
	return clampView({ lo: view.lo + d, hi: view.hi + d }, capture, minSpan);
}

/** A finished marquee: order the ends, clamp, enforce the min span. */
export function viewFromMarquee(aHz: number, bHz: number,
	capture: FreqWindow, minSpan: number): FreqWindow {
	const lo = Math.min(aHz, bHz), hi = Math.max(aHz, bHz);
	if (!(hi - lo >= minSpan)) {
		// Too narrow to be a selection: expand symmetrically around the midpoint.
		const mid = (lo + hi) / 2;
		return clampView({ lo: mid - minSpan / 2, hi: mid + minSpan / 2 }, capture, minSpan);
	}
	return clampView({ lo, hi }, capture, minSpan);
}

/** Frequency ↔ plot-rect x for the CURRENT view (the one mapping every renderer must use). */
export function freqToPlotX(fHz: number, view: FreqWindow, rect: { x: number; w: number }): number {
	return rect.x + (fHz - view.lo) / (view.hi - view.lo) * rect.w;
}

/** Inverse mapping (marker hit-test, tune-by-click). Not clamped: callers decide containment. */
export function plotXToFreq(x: number, view: FreqWindow, rect: { x: number; w: number }): number {
	return view.lo + (x - rect.x) / rect.w * (view.hi - view.lo);
}

/** Real samples inside the view (the sample-count hint; null when there is no axis yet). */
export function samplesInView(freq: ArrayLike<number> | null | undefined, view: FreqWindow): number | null {
	if (!freq || freq.length < 2) return null;
	let n = 0;
	for (let i = 0; i < freq.length; i++) {
		const f = freq[i];
		if (f >= view.lo && f <= view.hi) n++;
	}
	return n;
}

// ---------------- module state (capture + view) ----------------

/** The window of the frame on screen; null until a renderer has drawn a real axis. */
let capture: FreqWindow | null = null;
/** What the main plot shows; null = full capture. */
let view: FreqWindow | null = null;
/** Monotonic bump whenever either changes (cheap per-frame change check for the overview). */
let version = 0;

/** The drawn-data window (design §4.2): SWP freqArray ends, RTA/SDR startHz/stopHz. */
export function getCapture(): FreqWindow | null {
	return capture;
}

/** The effective view: the zoom window, or the full capture when not zoomed. */
export function getView(): FreqWindow | null {
	return view ?? capture;
}

/** True when the main plot currently shows a sub-window (not the whole capture). */
export function isZoomed(): boolean {
	return view !== null;
}

/** Bumped on every capture/view change (overview + hint redraw checks). */
export function viewportVersion(): number {
	return version;
}

/**
 * Feed the window of the frame the renderer is about to draw.
 *
 * Same window → nothing changes (the view survives). A NEW window → the view is dropped:
 * the zoom was selected against the old capture, and pasting it onto different data is
 * exactly the misalignment the design forbids (§4.3). Returns true when the viewport changed.
 */
export function setCapture(w: FreqWindow | null | undefined): boolean {
	if (!isValidWindow(w)) {
		if (capture === null && view === null) return false;
		capture = null;
		view = null;
		version++;
		return true;
	}
	const next = { lo: w.lo, hi: w.hi };
	if (sameWindow(next, capture)) return false;
	capture = next;
	view = null;
	version++;
	return true;
}

/** Apply a computed view (already clamped by the helpers). Null = back to full capture. */
export function setView(w: FreqWindow | null, minSpan?: number): boolean {
	if (w === null) {
		if (view === null) return false;
		view = null;
		version++;
		return true;
	}
	if (!isValidWindow(w) || !capture) return false;
	const next = { lo: w.lo, hi: w.hi };
	if (sameWindow(next, view ?? capture)) return false;
	// Keep the invariant here too: a raw setter cannot escape the capture.
	const clamped = clampView(next, capture, minSpan ?? (capture.hi - capture.lo) / 1000);
	// A view clamped back to the full window IS the full window (zoom-out must clear the
	// zoomed flag, not park a capture-sized view).
	view = sameWindow(clamped, capture) ? null : clamped;
	version++;
	return true;
}

/** Reset everything (mode change, Preset, disconnect). The toggle stays as the user set it. */
export function resetViewport(): void {
	if (capture === null && view === null) return;
	capture = null;
	view = null;
	version++;
}

/** For tests: seed a capture directly (production goes through the renderer/frame path). */
export function __testSeedCapture(w: FreqWindow | null): void {
	capture = w && isValidWindow(w) ? { lo: w.lo, hi: w.hi } : null;
	view = null;
	version++;
}
