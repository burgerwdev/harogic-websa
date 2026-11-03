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
 *   - nothing persists: a reload, a mode switch or a Preset returns to the full window.
 *     A translated capture (same width) keeps the view's width and its place in the band;
 *     a capture whose width changes drops the view (design §4.3).
 *
 * Window algebra is pure. Direct gestures apply immediately; automatic positioning uses a
 * short animation of this same window, so renderers and hit tests stay in sync. No canvas
 * or WebSocket is needed to drive this state.
 */
import { createParam } from '../core/params';
import { requestRender } from '../render/redraw';

export type FreqWindow = { lo: number; hi: number };

/**
 * Master zoom toggle (the top-bar button next to the keypad). A client-owned preference that
 * is deliberately NOT persisted (design §3: Off after a reload), so `authoritative` with no
 * persistKey: `get()` is whatever the user last clicked, `resetAll('zoom')` clears it.
 */
export const zoomEnabled = createParam<boolean>('zoom.enabled', {
	fallback: false, scope: 'zoom', authoritative: true,
});


export type LevelWindow = { top: number; bottom: number };
let levelView: LevelWindow | null = null;

/** Display-only raw dBm bounds; independent of reference and receiver settings. */
export function getLevelView(): LevelWindow | null { return levelView; }

export function setLevelView(top: number, bottom: number): boolean {
	if (!Number.isFinite(top) || !Number.isFinite(bottom) || top - bottom < 1) return false;
	if (levelView?.top === top && levelView.bottom === bottom) return false;
	levelView = { top, bottom };
	return true;
}

export function clearLevelView(): void { levelView = null; }
/** Two windows closer than this are the same window (per-frame float jitter must not reset). */
export const SAME_WINDOW_HZ = 0.5;
/** The marquee only becomes a zoom past this width (CSS px; the gesture layer measures it). */
export const MARQUEE_MIN_PX = 8;
/** Below this many samples in the view the UI shows the "no new detail" hint (design §3). */
export const DETAIL_HINT_SAMPLES = 8;
/** Short ease-out for automatic positioning; pointer drags always write immediately. */
export const VIEW_SLIDE_MS = 240;

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
/** Same width (within SAME_WINDOW_HZ). Ends may differ: that is a pan, not a new span. */
export function sameSpan(a: FreqWindow | null, b: FreqWindow | null): boolean {
	return !!a && !!b && Math.abs((a.hi - a.lo) - (b.hi - b.lo)) <= SAME_WINDOW_HZ;
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

/** Reveal a receiver and its passband with the smallest pan, keeping the zoom scale. */
export function panToReveal(view: FreqWindow, hz: number, band: FreqWindow,
	capture: FreqWindow, minSpan: number): FreqWindow {
	const lo = Math.max(capture.lo, Math.min(hz, band.lo));
	const hi = Math.min(capture.hi, Math.max(hz, band.hi));
	const span = view.hi - view.lo;
	// A band wider than the view cannot fit without changing the user's zoom.
	// Keep the dial visible instead (also covers an out-of-capture target).
	if (hi - lo > span || hi < lo) {
		return hz >= view.lo && hz <= view.hi ? clampView(view, capture, minSpan)
			: panToCenter(view, hz, capture, minSpan);
	}
	const shift = lo < view.lo ? lo - view.lo : hi > view.hi ? hi - view.hi : 0;
	return clampView({ lo: view.lo + shift, hi: view.hi + shift }, capture, minSpan);
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

/**
 * Nearest data index of an absolute frequency on the (increasing) capture axis — the
 * inverse of the display mapping for hit-tests (marker placement under a zoomed view).
 * Binary search: the axis can be 100k points and this runs per mousemove.
 */
export function nearestIdxForFreq(freq: ArrayLike<number>, fHz: number): number {
	const n = freq.length;
	if (n < 2) return 0;
	let lo = 0, hi = n - 1;
	while (hi - lo > 1) {
		const mid = (lo + hi) >> 1;
		if (freq[mid] < fHz) lo = mid; else hi = mid;
	}
	return Math.abs(freq[lo] - fHz) <= Math.abs(freq[hi] - fHz) ? lo : hi;
}

// ---------------- module state (capture + view) ----------------

/** The window of the frame on screen; null until a renderer has drawn a real axis. */
let capture: FreqWindow | null = null;
/** What the main plot shows; null = full capture. */
let view: FreqWindow | null = null;
let viewSlide: { from: FreqWindow; to: FreqWindow; start: number; frame: number; minSpan: number } | null = null;

function cancelViewSlide(): void {
	if (viewSlide) cancelAnimationFrame(viewSlide.frame);
	viewSlide = null;
}
/** Raw view and master-toggle state saved only while the passband focus is active. */
let passbandSnapshot: { view: FreqWindow | null; zoomEnabled: boolean } | null = null;
/** A relocated receiver is focused only once its acknowledged capture is drawn. */
type ReceiverFocus = { hz: number; expected: FreqWindow; minSpan: number; band?: FreqWindow };
let pendingRecenter: ReceiverFocus | null = null;
let receiverFocusGeneration = 0;

/** User view gestures supersede automatic focus awaiting a command acknowledgement. */
export function receiverFocusEpoch(): number { return receiverFocusGeneration; }

/** True while a second passband-view action will restore the saved display state. */
export function passbandViewActive(): boolean {
	return passbandSnapshot !== null;
}

/** Forget the saved display state without moving the current view (mode/disconnect). */
export function clearPassbandView(): void {
	if (passbandSnapshot) {
		passbandSnapshot = null;
		version++;
	}
}
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

/** Apply an acknowledged receiver relocation after its matching acquisition frame arrives. */
export function recenterOnCapture(hz: number, expected: FreqWindow, minSpan: number): void {
	focusOnCapture({ hz, expected, minSpan });
}

/** A new axis gesture supersedes receiver following that is waiting for a frame. */
export function cancelReceiverFocus(): void {
	receiverFocusGeneration++;
	pendingRecenter = null;
	cancelViewSlide();
}

/** Follow tuning only when its dial/passband would leave the visible window. */
export function revealOnCapture(hz: number, band: FreqWindow, expected: FreqWindow, minSpan: number): void {
	if (!isValidWindow(band)) return;
	focusOnCapture({ hz, band: { ...band }, expected, minSpan });
}

function focusedView(current: FreqWindow, focus: ReceiverFocus): FreqWindow {
	const minimum = Math.min(focus.minSpan, focus.expected.hi - focus.expected.lo);
	return focus.band
		? panToReveal(current, focus.hz, focus.band, focus.expected, minimum)
		: panToCenter(current, focus.hz, focus.expected, minimum);
}

function focusOnCapture(focus: ReceiverFocus): void {
	const { hz, expected, minSpan } = focus;
	pendingRecenter = null;
	if (!view || !Number.isFinite(hz) || !isValidWindow(expected)
		|| !Number.isFinite(minSpan) || minSpan <= 0) return;
	if (sameWindow(capture, expected)) {
		const minimum = Math.min(minSpan, expected.hi - expected.lo);
		const next = focusedView(view, focus);
		if (!sameWindow(next, view)) slideViewTo(next, minimum);
	} else {
		cancelViewSlide();
		pendingRecenter = { ...focus, expected: { ...expected } };
	}
}

/**
 * Feed the window of the frame the renderer is about to draw.
 *
 * Same window → nothing changes. A translated window of the same width (an x-axis
 * band pan) shifts the view by the same amount: the zoom box is pinned to the
 * observation FRAME, the spectrum (and the listen flag on it) slides under it,
 * clamped only at the frame's edges. Confirmed SDR tuning in controls.ts
 * follows an off-screen receiver once the matching capture arrives; intermediate pans
 * keep that pending target. Axis pans retain their frame-relative view without receiver
 * focus. A window whose width changed drops the view: that zoom
 * was selected against a different span. Returns true when the viewport changed.
 */
export function setCapture(w: FreqWindow | null | undefined, spanToleranceHz = SAME_WINDOW_HZ): boolean {
	if (!isValidWindow(w)) {
		cancelViewSlide();
		pendingRecenter = null;
		clearPassbandView();
		if (capture === null && view === null) return false;
		capture = null;
		view = null;
		version++;
		return true;
	}
	const next = { lo: w.lo, hi: w.hi };
	if (sameWindow(next, capture)) return false;
	cancelViewSlide();
	const recenter = pendingRecenter;
	pendingRecenter = null;
	clearPassbandView();
	const prev = capture;
	capture = next;
	if (view && prev && Math.abs((next.hi - next.lo) - (prev.hi - prev.lo)) <= spanToleranceHz) {
		const shift = next.lo - prev.lo;
		const shifted = clampView(
			{ lo: view.lo + shift, hi: view.hi + shift }, next,
			Math.min(view.hi - view.lo, next.hi - next.lo));
		view = sameWindow(shifted, next) ? null : shifted;
	} else {
		view = null;
	}
	if (view && recenter && sameWindow(next, recenter.expected)) {
		const centered = focusedView(view, recenter);
		slideViewTo(centered, Math.min(recenter.minSpan, next.hi - next.lo));
	} else if (view && recenter && sameSpan(next, recenter.expected)) {
		// Rapid tuning may confirm the newest target before older acquisition frames
		// drain. Only its matching frame may consume the deferred focus.
		pendingRecenter = recenter;
	}
	version++;
	return true;
}

/** Apply a computed view (already clamped by the helpers). Null = back to full capture. */
export function setView(w: FreqWindow | null, minSpan?: number): boolean {
	cancelReceiverFocus();
	// Every external view action makes an earlier passband snapshot stale, even when the
	// requested window happens to equal the current one.
	clearPassbandView();
	return applyView(w, minSpan);
}

/** Pan the actual view, so the overview, spectrum, waterfall and hit tests share every frame. */
export function slideViewTo(w: FreqWindow, minSpan: number): boolean {
	if (!capture || !isValidWindow(w) || !Number.isFinite(minSpan) || minSpan <= 0) return false;
	const target = clampView(w, capture, minSpan);
	if (viewSlide && sameWindow(viewSlide.to, target)) return false;
	cancelViewSlide();
	if (!view || !sameSpan(view, target) || sameWindow(view, target)
		|| typeof requestAnimationFrame !== 'function'
		|| window.matchMedia?.('(prefers-reduced-motion: reduce)').matches || document.hidden) {
		return setView(target, minSpan);
	}
	pendingRecenter = null;
	clearPassbandView();
	const slide = { from: { ...view }, to: target, start: performance.now(), frame: 0, minSpan };
	viewSlide = slide;
	const tick = (now: number) => {
		if (viewSlide !== slide) return;
		const reduced = document.hidden || window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
		const progress = reduced ? 1 : Math.max(0, Math.min(1, (now - slide.start) / VIEW_SLIDE_MS));
		const ease = 1 - Math.pow(1 - progress, 3);
		applyView(progress === 1 ? slide.to : {
			lo: slide.from.lo + (slide.to.lo - slide.from.lo) * ease,
			hi: slide.from.hi + (slide.to.hi - slide.from.hi) * ease,
		}, slide.minSpan);
		if (progress === 1) viewSlide = null;
		requestRender();
		if (viewSlide === slide) slide.frame = requestAnimationFrame(tick);
	};
	slide.frame = requestAnimationFrame(tick);
	return true;
}

/** Internal view write which can preserve the snapshot being entered or restored. */
function applyView(w: FreqWindow | null, minSpan?: number): boolean {
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

/**
 * Focus the passband, or restore the exact raw view and master zoom preference saved on entry.
 * Returns the resulting focus state. No acquisition or receiver parameter is touched.
 */
export function togglePassbandView(target: FreqWindow, minSpan: number): boolean {
	cancelReceiverFocus();
	if (passbandSnapshot) {
		const saved = passbandSnapshot;
		passbandSnapshot = null;
		// The snapshot was already clamped on entry. Restore it verbatim rather than
		// re-clamping against a potentially newer frequency-grid minimum span.
		view = saved.view ? { lo: saved.view.lo, hi: saved.view.hi } : null;
		version++;
		zoomEnabled.set(saved.zoomEnabled);
		return false;
	}
	if (!capture || !isValidWindow(target)) return false;
	passbandSnapshot = {
		view: view ? { lo: view.lo, hi: view.hi } : null,
		zoomEnabled: zoomEnabled.get(),
	};
	applyView(target, minSpan);
	zoomEnabled.set(true);
	return true;
}

/** Reset everything (mode change, Preset, disconnect). The toggle stays as the user set it. */
export function resetViewport(): void {
	cancelReceiverFocus();
	clearLevelView();
	clearPassbandView();
	if (capture === null && view === null) return;
	capture = null;
	view = null;
	version++;
}

/** For tests: seed a capture directly (production goes through the renderer/frame path). */
export function __testSeedCapture(w: FreqWindow | null): void {
	cancelViewSlide();
	pendingRecenter = null;
	clearPassbandView();
	capture = w && isValidWindow(w) ? { lo: w.lo, hi: w.hi } : null;
	view = null;
	version++;
}
