/**
 * Axis-label gestures: pan and zoom the window a spectrum plot is drawn in.
 *
 * Bench analysers with a touch surface let you grab the frequency axis and the level axis and
 * push them around, and every SDR panadapter does the same. That is the gesture added here, one
 * band per axis (the bands are the margins outside the graticule; `axisBandAt` in controls.ts
 * owns the hit test):
 *
 *   * x band (the frequency row under the graticule): drag left/right pans the centre frequency,
 *     wheel zooms the span around the pointer,
 *   * y band (the level labels right of the graticule): drag up/down pans the reference level,
 *     wheel zooms dB/div around the pointer.
 *
 * The content follows the finger: dragging the frequency row to the right slides the trace to the
 * right and shows lower frequencies, dragging the level labels down moves the trace down and
 * raises the reference. That is the direct-manipulation convention (SDR#, GQRX, SDRangel, and the
 * touch axes of a bench analyser).
 *
 * WHY A PREVIEW, AND WHY IT DOES NOT SNAP BACK
 *
 * One frequency request costs a device reconfiguration (0.3-1 s measured) and one Ref change costs
 * ~1.9 s end to end, so following the pointer with real reconfigurations cannot be smooth - and
 * every reconfiguration restarts the sweep, so the trace would flicker to an empty canvas while
 * it lands. Nothing is therefore sent while the pointer moves: the plot is redrawn locally by
 * transforming the mapping the renderers already go through (`getX`/`getY` in render/plot.ts),
 * and exactly one request goes out when the gesture settles (pointer up, or 250 ms of stillness,
 * never more often than once per MIN_COMMIT_INTERVAL_MS).
 *
 * The transform is expressed against the window the DATA on screen was measured in
 * (`committedFreqWindow` - the ends of the drawn frequency array / the RTA frame window), not
 * against a snapshot taken at pointer-down. So when the confirming STATUS arrives the two windows
 * coincide, the transform becomes the identity on its own, and the trace never jumps back to the
 * old window while the device is still reconfiguring. `refresh()` drops the preview when the
 * drawn window matches, or the requested display Ref is acknowledged and shown.
 *
 * The absolute (dBm) level axis follows the user's requested Ref, even when hardware gain limits
 * produce a different effective Ref. A drag previews and commits that request inside the manual
 * -160..40 dBm hardware bounds shifted by the level offset. In relative dB the top is pinned to 0,
 * so the same drag pans the level OFFSET (`commitLevelOffset`), the value that actually moves that
 * axis. That one is
 * the client's own display value, so it is applied as the pointer moves: nothing to wait for,
 * nothing to commit. Zooming dB/div is client-side in both displays, so the y wheel applies
 * immediately through the Level panel's own setter.
 */
import * as S from '../core/store';
import { normalizeCenterSpan } from '../core/frequency';
import { send } from '../core/wsSend';
import { currentGraphMode } from './graphMode';
import { centerHz, spanHz } from './freqState';
import { clampDisplayRef, clearDisplayBounds, getDisplayRange, getDisplayTop, hardwareRefForDisplay, hasDisplayBounds, pendingDisplayRef, setDisplayRefShiftHandler } from './displayRef';
import { displayOffset, displayUnit } from './displayState';
import { estimatedCaptureSpanHz } from './sdrState';
import { getView, isZoomed } from './spectrumViewport';
import { cancelSdrTuneRequest } from './sdrTuneRequest';
import { beginFrequencyCommit } from './panels/frequency';
import { beginRtaAxisTransition } from './panels/rta';
import { commitLevelOffset, commitRefLevel, setScale } from './panels/refAmp';

export type Axis = 'x' | 'y';
export type FreqWindow = { lo: number; hi: number };
export type LevelWindow = { ref: number; range: number };
export type Rect = { x: number; y: number; w: number; h: number };

/** A still pointer for this long commits its current target without ending a held drag. */
export const IDLE_COMMIT_MS = 250;
/** Two commits are never closer than this: one Ref change reconfigures the front end. */
export const MIN_COMMIT_INTERVAL_MS = 400;
/** A wheel spin (then another) this soon is the same gesture, so it keeps its accumulated zoom. */
const CHAIN_MS = 1500;
/** How long a committed preview is kept when the device never reports the requested window. */
const SETTLE_MAX_MS = 6000;
/** The drawn window counts as "the requested one" inside these tolerances. */
const FREQ_MATCH_FRAC = 2e-3;
const LEVEL_MATCH_DB = 0.6;
/** dB/div limits for the y wheel (the Level panel's presets are 1-10, the extremes are padded). */
const MIN_DB_PER_DIV = 0.5;
const MAX_DB_PER_DIV = 20;
/** The preview is kept inside the instrument's own window (the commit clamps the same way). */
const MIN_SPAN_HZ = 100;

type GestureBase = { freq: FreqWindow | null; level: LevelWindow };
type Settle = { freq?: FreqWindow; level?: LevelWindow; levelAccepted?: boolean };
/** What the level axis changes: the device reference, or the relative display's own offset. */
type LevelTarget = 'ref' | 'offset';

type Session = {
	axis: Axis;
	rect: Rect;
	x0: number;
	y0: number;
	/** What the pointer deltas and wheel factors are applied to (see CHAIN_MS). */
	base: GestureBase;
	/** Display window at gesture start; capture requests retain their full IQ span. */
	displayBase: FreqWindow | null;
	/** Native axis at press time; quantization tolerance cannot settle against old data. */
	frequencyGrid: Float64Array | null;
	/** Wheel-zoom anchor as a fraction of the band, so the value under the pointer stays put. */
	anchor: number;
	zoomFreq: number;
	zoomLevel: number;
	targetFreq: FreqWindow | null;
	targetLevel: LevelWindow;
	/** What the level axis means in the display unit on screen (see the header). */
	level: LevelTarget;
	/** The offset the gesture started from (only meaningful for `level: 'offset'`). */
	baseOffset: number;
	/** The last pointer target sent in this session; idle commits do not reset the drag origin. */
	committedRef: number;
	/**
	 * The pointer is HELD ON A BAND: this is what makes it a gesture. A wheel session is live
	 * without it, so hovering the canvas afterwards neither pans anything nor swallows the
	 * in-plot gestures (marker placement, the SDR tune drag) that a release may belong to.
	 */
	down: boolean;
	/** The pointer is held or a wheel is still settling (blocks the preview auto-drop). */
	live: boolean;
	/** What was requested and has not been seen on screen yet. */
	settle: Settle | null;
};

let session: Session | null = null;
/** The target of the last gesture, so a second spin/drag in quick succession continues it. */
let chain: (GestureBase & { axis: Axis; at: number }) | null = null;
let idleTimer: number | null = null;
let settleTimer: number | null = null;
let lastCommitAt = 0;
let sdrSerial = 0;
let sdrInFlight: { id: string; freq: FreqWindow; revision: number; timer: number } | null = null;
let sdrQueued: FreqWindow | null = null;

// ---------------- the windows on screen ----------------

/**
 * The frequency window the trace on screen was measured in.
 *
 * For the swept modes that is the drawn frequency array (which is what the axis row is labelled
 * from, and what a non-uniform device grid would differ in), falling back to the requested
 * centre/span before the first frame. RTA and SDR are aligned to their display window by the
 * STATUS handler, so the frame's own start/stop is the drawn window there.
 */
export function committedFreqWindow(): FreqWindow | null {
	const mode = currentGraphMode();
	if (mode === 'std') {
		const fa = S.freqArray;
		if (fa && fa.length > 1 && fa[fa.length - 1] > fa[0]) {
			return { lo: fa[0], hi: fa[fa.length - 1] };
		}
		const c = centerHz.get();
		const span = spanHz.get();
		if (!(c > 0) || !(span > 0)) return null;
		return { lo: c - span / 2, hi: c + span / 2 };
	}
	const d = S.rtaData;
	if (!d || !(d.stopHz > d.startHz)) return null;
	return { lo: d.startHz, hi: d.stopHz };
}

/** The level window the graticule shows: the top line and its height. */
export function committedLevelWindow(): LevelWindow {
	return { ref: getDisplayTop(), range: Math.max(1e-6, getDisplayRange()) };
}

/** The level at a fraction of the plot height (0 = top edge). */
function levelAt(frac: number, w: LevelWindow = committedLevelWindow()): number {
	return w.ref - frac * w.range;
}

function clampFreq(w: FreqWindow): FreqWindow {
	if (currentGraphMode() === 'sdr') {
		// RF limits constrain the tuner center; the fixed IQ window may cross either limit.
		const half = (w.hi - w.lo) / 2;
		const center = Math.max(S.FREQ_MIN, Math.min(S.FREQ_MAX, (w.lo + w.hi) / 2));
		return { lo: center - half, hi: center + half };
	}
	const n = normalizeCenterSpan(
		(w.lo + w.hi) / 2, w.hi - w.lo, S.FREQ_MIN, S.FREQ_MAX, MIN_SPAN_HZ);
	if (!n) return w;
	return { lo: n.start, hi: n.stop };
}

const near = (a: number, b: number, tol: number) => Math.abs(a - b) <= tol;

/**
 * Which axis-label band a canvas point is in, if any.
 *
 * The bands are the margins the plot geometry already reserves: the frequency row is drawn under
 * the graticule (MARGIN.bottom) and the level labels to its right (MARGIN.right). Both are inside
 * the canvas and outside `plotRect`, which is also why they cannot shadow the gestures that start
 * inside the plot: marker placement, the SDR tune drag and Shift+click all return early when the
 * pointer is outside the plot rectangle.
 */
export function axisBandAt(x: number, y: number, rect: Rect): Axis | null {
	if (y > rect.y + rect.h) return 'x';           // the bottom-right corner belongs to this row
	if (x > rect.x + rect.w) return 'y';
	return null;
}

// ---------------- what the renderers read ----------------

/**
 * Drop a preview the screen has caught up with.
 *
 * Called from the getters (i.e. once per frame) so the check costs nothing extra: as soon as the
 * drawn data window and the display ref match the request, the preview has done its job.
 */
function refresh(): void {
	const s = session;
	if (!s || s.down || s.live || !s.settle) return;
	if (s.axis === 'x' && sdrInFlight) return;
	const want = s.settle;
	let done = true;
	if (want.freq) {
		const drawn = committedFreqWindow();
		const span = want.freq.hi - want.freq.lo;
		const grid = currentGraphMode() === 'std' && S.freqArray !== s.frequencyGrid ? S.freqArray : null;
		const edgeStep = grid && grid.length > 1 ? Math.max(
			grid[1] - grid[0], grid[grid.length - 1] - grid[grid.length - 2]) : 0;
		const tol = Math.max(1, FREQ_MATCH_FRAC * span, 2 * edgeStep);
		done = drawn !== null
			&& near(drawn.lo, want.freq.lo, tol) && near(drawn.hi, want.freq.hi, tol);
	}
	if (done && want.level) {
		const drawn = committedLevelWindow();
		done = want.levelAccepted === true && near(drawn.ref, want.level.ref, LEVEL_MATCH_DB)
			&& near(drawn.range, want.level.range, LEVEL_MATCH_DB);
	}
	if (done) clearPreview();
}

/** Acquisition target for overview feedback; never replaces the committed capture. */
export function previewCaptureWindow(): FreqWindow | null {
	refresh();
	return session?.axis === 'x' ? session.targetFreq : null;
}

/** The frequency window to draw and label right now (null when nothing is being previewed). */
export function previewFreqWindow(): FreqWindow | null {
	refresh();
	const s = session;
	if (!s || s.axis !== 'x' || !s.targetFreq) return null;
	if (!s.displayBase || !s.base.freq) return s.targetFreq;
	const scale = (s.targetFreq.hi - s.targetFreq.lo) / (s.base.freq.hi - s.base.freq.lo);
	return {
		lo: s.targetFreq.lo + (s.displayBase.lo - s.base.freq.lo) * scale,
		hi: s.targetFreq.lo + (s.displayBase.hi - s.base.freq.lo) * scale,
	};
}

/** The level window to draw and label right now (null when nothing is being previewed). */
export function previewLevelWindow(): LevelWindow | null {
	refresh();
	return session ? session.targetLevel : null;
}

/** Only the stored requested Ref acknowledges the display; effective SDK Ref may differ. */
export function confirmAxisRef(requested: unknown): void {
	const s = session;
	if (!s || s.axis !== 'y' || s.level !== 'ref' || !s.settle?.level
		|| typeof requested !== 'number' || !near(requested, hardwareRefForDisplay(s.settle.level.ref), 0.05)) return;
	s.settle.levelAccepted = true;
	refresh();
}

/** Keep pending and held gestures on the same raw window when compensation changes. */
export function shiftAxisRef(delta: number): void {
	const s = session;
	if (s && s.level === 'ref') {
		s.base.level = { ...s.base.level, ref: s.base.level.ref + delta };
		s.targetLevel = { ...s.targetLevel, ref: s.targetLevel.ref + delta };
		s.committedRef += delta;
		if (s.settle?.level) s.settle.level = { ...s.settle.level, ref: s.settle.level.ref + delta };
	}
	if (chain) chain.level = { ...chain.level, ref: chain.level.ref + delta };
}

// Offset compensation reaches this gesture through displayRef, not a panel import.
setDisplayRefShiftHandler(shiftAxisRef);

/** A rejected Ref must not leave a preview hiding the restored display. */
export function rejectAxisRef(): void {
	if (session?.axis === 'y' && session.level === 'ref' && session.settle?.level) clearPreview();
}

/** Only the matching command may settle a pan; older reports must not move newer input. */
export function confirmSdrAxisWindow(center: number, span: number, revision: number,
	responseTo: unknown, requestId: unknown): void {
	const pending = sdrInFlight;
	if (currentGraphMode() !== 'sdr' || !pending || responseTo !== 'SET_SDR'
		|| requestId !== pending.id || revision < pending.revision
		|| !Number.isFinite(revision) || !Number.isFinite(center) || !(span > 0)) return;
	window.clearTimeout(pending.timer);
	sdrInFlight = null;
	const accepted = { lo: center - span / 2, hi: center + span / 2 };
	const s = session;
	if (s?.axis === 'x' && s.settle?.freq === pending.freq) {
		s.settle.freq = accepted;
		// A released, unchanged target follows the device's passband fit. Input that arrived
		// during the round trip keeps its absolute target and pointer origin.
		if (!s.down && !s.live && s.targetFreq
			&& near(s.targetFreq.lo, pending.freq.lo, 1) && near(s.targetFreq.hi, pending.freq.hi, 1)) {
			s.targetFreq = accepted;
			if (chain?.axis === 'x') chain.freq = accepted;
		}
	}
	const next = sdrQueued;
	sdrQueued = null;
	if (next) submitSdrWindow(next);
	refresh();
	if (session) scheduleSettleTimeout();
}

/** A failed pan discards unsent input; its error remains visible through the WS handler. */
export function rejectSdrAxisWindow(responseTo?: string, requestId?: string | number): void {
	if (responseTo !== 'SET_SDR' || requestId !== sdrInFlight?.id) return;
	cancelSdrWindow();
	if (session?.axis === 'x') clearPreview();
	if (chain?.axis === 'x') chain = null;
}

function cancelSdrWindow(): void {
	if (sdrInFlight) window.clearTimeout(sdrInFlight.timer);
	sdrInFlight = null;
	sdrQueued = null;
}

/**
 * Pixel transform for the frequency axis: `x' = x0 + (x - x0) * sx + dxFrac * width`.
 *
 * Returns null when there is nothing to preview or the transform is the identity, so the
 * non-gesture path through `getX`/`getY` is bit-for-bit what it always was.
 */
export function xAxisTransform(): { sx: number; dxFrac: number } | null {
	refresh();
	const s = session;
	if (!s || s.axis !== 'x' || !s.targetFreq) return null;
	const base = s.displayBase ? getView() : committedFreqWindow();
	const target = previewFreqWindow();
	if (!base || !target) return null;
	const baseSpan = base.hi - base.lo;
	const span = target.hi - target.lo;
	if (!(baseSpan > 0) || !(span > 0)) return null;
	const sx = baseSpan / span;
	const dxFrac = (base.lo - target.lo) / span;
	if (near(sx, 1, 1e-6) && near(dxFrac, 0, 1e-9)) return null;
	return { sx, dxFrac };
}

/** Pixel transform for the level axis: `y' = y0 + (y - y0) * sy + dyFrac * height`. */
export function yAxisTransform(): { sy: number; dyFrac: number } | null {
	refresh();
	const s = session;
	if (!s || s.axis !== 'y') return null;
	const base = committedLevelWindow();
	const { ref, range } = s.targetLevel;
	if (!(range > 0)) return null;
	const sy = base.range / range;
	const dyFrac = (ref - base.ref) / range;
	if (near(sy, 1, 1e-6) && near(dyFrac, 0, 1e-9)) return null;
	return { sy, dyFrac };
}

/** True while a gesture is in progress (drives the canvas cursor and the drag bookkeeping). */
export function axisDragging(): boolean {
	return session !== null && session.down;
}

/** Forget any preview (mode change, preset, or a failed gesture). */
export function resetAxisPreview(): void {
	cancelSdrWindow();
	clearPreview();
	chain = null;
	// The rate limiter's memory of the last commit belongs to the gesture that ended, not to the
	// next one: the caller that clears the preview may be a different mode with its own cadence.
	lastCommitAt = 0;
}

/** A display slider supersedes an unfinished level gesture without touching receiver intent. */
export function cancelLevelPreview(): void {
	if (session?.axis === 'y') clearPreview();
	if (chain?.axis === 'y') chain = null;
}

// ---------------- the gesture ----------------

function clearTimers(): void {
	if (idleTimer !== null) { window.clearTimeout(idleTimer); idleTimer = null; }
	if (settleTimer !== null) { window.clearTimeout(settleTimer); settleTimer = null; }
}

function clearPreview(): void {
	clearTimers();
	session = null;
}

function chainBase(axis: Axis): GestureBase | null {
	// Ref and dB/div writes update the client display immediately. Reusing a cached Y target
	// would override newer panel edits, request bounds, or a rejected SDR request's rollback.
	if (axis === 'y') return null;
	if (session?.axis === 'x' && session.targetFreq && sdrInFlight)
		return { freq: session.targetFreq, level: session.targetLevel };
	if (!chain || chain.axis !== axis) return null;
	if (performance.now() - chain.at > CHAIN_MS) return null;
	return { freq: chain.freq, level: chain.level };
}

function rememberTarget(): void {
	const s = session;
	if (!s || s.axis === 'y') return;
	chain = { axis: s.axis, freq: s.targetFreq, level: s.targetLevel, at: performance.now() };
}

/**
 * Start a gesture on an axis band. Returns false when the gesture is not available (no drawn
 * window yet, or the relative display unit for the level axis), in which case the caller leaves
 * the pointer event to whatever else handles it.
 */
export function axisDragStart(axis: Axis, px: number, py: number, rect: Rect): boolean {
	return beginSession(axis, px, py, rect, true);
}

/**
 * Open a session.
 *
 * `held` distinguishes the two entry points: a press on the band (the pointer is down, so
 * movements steer it and only the release commits) and a wheel, which has no pointer state at all.
 */
function beginSession(axis: Axis, px: number, py: number, rect: Rect, held: boolean): boolean {
	if (axis === 'y' && hasDisplayBounds()) {
		clearDisplayBounds();
		cancelLevelPreview();
	}
	const freq = committedFreqWindow();
	if (axis === 'x' && !freq) return false;
	const level = committedLevelWindow();
	const chained = chainBase(axis);
	const base: GestureBase = chained
		? { freq: chained.freq ?? freq, level: chained.level }
		: { freq, level };
	const pending = session?.axis === axis && (axis === 'y' || currentGraphMode() === 'sdr')
		? session.settle : null;
	const display = axis === 'x' && isZoomed()
		? previewFreqWindow() ?? getView() : null;
	clearTimers();
	session = {
		axis, rect, x0: px, y0: py,
		base,
		displayBase: display ? { ...display } : null,
		frequencyGrid: S.freqArray,
		anchor: axis === 'x' ? (px - rect.x) / rect.w : (py - rect.y) / rect.h,
		zoomFreq: 1,
		zoomLevel: 1,
		targetFreq: base.freq,
		targetLevel: base.level,
		// The absolute display has a reference level; the relative one has no device reference on
		// its axis at all (the top is pinned to 0), so the same gesture moves the level offset -
		// which is the value that actually pans that axis, and is client-side.
		level: displayUnit.get() === 'dB' ? 'offset' : 'ref',
		baseOffset: displayOffset.get(),
		committedRef: base.level.ref,
		down: held,
		live: true,
		settle: pending,
	};
	return true;
}

/** Apply the pointer position: the target window is always derived from the whole delta. */
function applyPointer(px: number, py: number): void {
	const s = session;
	if (!s) return;
	if (s.axis === 'x' && s.base.freq) {
		const b = s.base.freq;
		const bSpan = b.hi - b.lo;
		const span = bSpan * s.zoomFreq;
		// Freeze the display scale at press time: incoming frames must not change
		// the frequency distance represented by an unchanged pointer delta.
		const rate = s.displayBase && bSpan > 0
			? (s.displayBase.hi - s.displayBase.lo) / bSpan : 1;
		const anchorHz = b.lo + s.anchor * bSpan;
		const lo = anchorHz - s.anchor * span - ((px - s.x0) / s.rect.w) * span * rate;
		s.targetFreq = clampFreq({ lo, hi: lo + span });
		return;
	}
	const bl = s.base.level;
	const range = bl.range * s.zoomLevel;
	const anchorDb = levelAt(s.anchor, bl);
	const ref = anchorDb + s.anchor * range + ((py - s.y0) / s.rect.h) * range;
	if (s.level === 'offset') {
		// Relative display: the gesture pans the level OFFSET. It is the client's own display
		// value, so it is applied as the pointer moves (nothing to wait for, nothing to commit)
		// and the target window stays where it was - the preview layer is not involved at all.
		s.targetLevel = bl;
		commitLevelOffset(s.baseOffset - (ref - bl.ref));
		return;
	}
	// Match commitRefLevel's manual bounds without limiting the relative display's offset.
	s.targetLevel = { ref: clampDisplayRef(ref), range };
}

export function axisDragMove(px: number, py: number): void {
	const s = session;
	// `down`, not `live`: an idle commit in the middle of a slow drag ends the debounce but not the
	// gesture, so the pointer has to keep steering it afterwards.
	if (!s || !s.down) return;
	s.live = true;
	applyPointer(px, py);
	scheduleIdleCommit();
}

export function axisDragEnd(): void {
	const s = session;
	if (!s || !s.down) return;
	s.down = false;
	commit('end');
}

/**
 * Wheel on an axis band: zoom around the pointer (the value under it stays put).
 *
 * The level zoom is a client-side display setting, so it is applied immediately - there is no
 * device round trip to hide, and the Refresh rate is the only thing that limits it. The frequency
 * zoom changes what the hardware sweeps, so it previews like a drag.
 */
export function axisWheel(axis: Axis, deltaY: number, px: number, py: number, rect: Rect): void {
	// A wheel while the pointer is held belongs to that gesture; a wheel on its own reuses the
	// session it started until the debounce commits it (and after that, CHAIN_MS keeps the
	// accumulated zoom through the next spin).
	const live = session !== null && session.live && session.axis === axis;
	if (!live && !beginSession(axis, px, py, rect, false)) return;
	const s = session!;
	s.anchor = axis === 'x' ? (px - rect.x) / rect.w : (py - rect.y) / rect.h;
	const factor = Math.exp(Math.max(-100, Math.min(100, deltaY)) * 0.0015);
	if (axis === 'x') {
		// The wheel caller drops local zoom even when it reuses a held pan session.
		// Stop applying that session's magnifier to the full capture preview.
		if (!isZoomed()) s.displayBase = null;
		s.zoomFreq = Math.max(0.02, Math.min(50, s.zoomFreq * factor));
		applyPointer(px, py);
	} else {
		const base = s.base.level;
		const range = Math.max(
			MIN_DB_PER_DIV * S.totalDivs,
			Math.min(MAX_DB_PER_DIV * S.totalDivs, base.range * s.zoomLevel * factor));
		s.zoomLevel = range / base.range;
		applyPointer(px, py);
		// dB/div is the client's own scale: no preview, no request. setScale keeps the Level panel,
		// the info bar and the density grid in step with it.
		setScale(s.targetLevel.range / S.totalDivs);
	}
	s.live = true;
	scheduleIdleCommit();
}

/** How many gestures actually committed a request (diagnostic for the e2e, `dataset.axisCommits`). */
let commitCount = 0;

function noteCommit(): void {
	commitCount += 1;
	const cv = document.getElementById('spectrum');
	const value = String(commitCount);
	if (cv && cv.dataset.axisCommits !== value) cv.dataset.axisCommits = value;
}

function scheduleIdleCommit(): void {
	if (idleTimer !== null) window.clearTimeout(idleTimer);
	idleTimer = window.setTimeout(() => { idleTimer = null; commit('idle'); }, IDLE_COMMIT_MS);
}

function targetMoved(): boolean {
	const s = session!;
	if (s.axis === 'x') {
		const b = currentGraphMode() === 'sdr' ? s.settle?.freq ?? s.base.freq : s.base.freq;
		const t = s.targetFreq;
		if (!b || !t) return false;
		const tol = Math.max(1, 1e-6 * (b.hi - b.lo));
		return !near(t.lo, b.lo, tol) || !near(t.hi, b.hi, tol);
	}
	if (s.level === 'offset') return false;         // applied live; there is no request to make
	return !near(s.targetLevel.ref, s.committedRef, 0.05);
}

/**
 * Send the requested window.
 *
 * `reason` is 'end' for a pointer release or 'idle' for a still pointer / a settled wheel; only a
 * release may exceed the minimum interval between commits, so a slow drag converges without
 * hammering the front end.
 */
function commit(reason: 'end' | 'idle'): void {
	const s = session;
	if (!s) return;
	if (idleTimer !== null) { window.clearTimeout(idleTimer); idleTimer = null; }
	if (!targetMoved()) {
		s.live = false;
		rememberTarget();
		if (!s.down && !s.settle) clearPreview();
		else if (s.settle) { refresh(); if (session) scheduleSettleTimeout(); }
		return;
	}
	const now = performance.now();
	if (reason === 'idle' && lastCommitAt !== 0 && now - lastCommitAt < MIN_COMMIT_INTERVAL_MS) {
		scheduleIdleCommit();
		return;
	}
	const settle: Settle = {};
	if (s.axis === 'x' && s.targetFreq) {
		const sent = commitFreqWindow(s.targetFreq);
		if (sent) {
			settle.freq = sent;
		}
	} else if (s.axis === 'y' && s.level === 'ref') {
		const sent = commitRefLevel(s.targetLevel.ref);
		if (sent !== null) {
			settle.level = { ref: sent, range: s.targetLevel.range };
			settle.levelAccepted = pendingDisplayRef() === null;
			s.committedRef = s.targetLevel.ref;
		}
	}
	lastCommitAt = now;
	if ((settle.freq && currentGraphMode() !== 'sdr') || settle.level) noteCommit();
	s.live = false;
	s.settle = settle.freq || settle.level ? settle : null;
	rememberTarget();
	scheduleSettleTimeout();
}

function scheduleSettleTimeout(): void {
	if (settleTimer !== null) { window.clearTimeout(settleTimer); settleTimer = null; }
	// An idle commit leaves the pointer in charge; its preview may expire only after release.
	if (session?.down || session?.live) return;
	if (sdrInFlight) return;
	settleTimer = window.setTimeout(() => {
		settleTimer = null;
		if (!session?.down && !session?.live) clearPreview();
	}, SETTLE_MAX_MS);
}

// ---------------- the requests ----------------

/** Nearest value of a `<select>`'s options, so a snapped window matches the panel's own list. */
function nearestOption(id: string, target: number): number | null {
	const sel = document.getElementById(id) as HTMLSelectElement | null;
	if (!sel) return null;
	let best: number | null = null;
	for (const option of Array.from(sel.options)) {
		const value = Number(option.value);
		if (!isFinite(value)) continue;
		if (best === null || Math.abs(value - target) < Math.abs(best - target)) best = value;
	}
	return best;
}

/** Select the option so the panel shows the value that was actually requested. */
function selectOption(id: string, value: number): void {
	const sel = document.getElementById(id) as HTMLSelectElement | null;
	if (sel) sel.value = String(value);
}

/** RTA and SDR spans are lists of hardware steps; the nearest one is what the device can do. */
function nearestDecimate(span: number): number | null {
	const sel = document.getElementById('select-sdr-decimate') as HTMLSelectElement | null;
	if (!sel) return null;
	let best: number | null = null;
	for (const option of Array.from(sel.options)) {
		const decimate = Number(option.value);
		if (!isFinite(decimate) || decimate <= 0) continue;
		if (best === null
			|| Math.abs(estimatedCaptureSpanHz(decimate) - span)
				< Math.abs(estimatedCaptureSpanHz(best) - span)) best = decimate;
	}
	return best;
}

/**
 * Ask for a frequency window. Returns the window that was requested (after the range clamp and,
 * for RTA/SDR, the step the hardware offers), which is what the preview then waits for.
 */
function commitFreqWindow(w: FreqWindow): FreqWindow | null {
	const mode = currentGraphMode();
	const center = (w.lo + w.hi) / 2;
	if (mode === 'std') {
		const n = normalizeCenterSpan(center, w.hi - w.lo, S.FREQ_MIN, S.FREQ_MAX);
		if (!n) return null;
    const request_id = beginFrequencyCommit('swp-freq-settings', n);
    send({ cmd: 'SET_FREQ', center: n.center, span: n.span, request_id });
		return { lo: n.start, hi: n.stop };
	}
	if (mode === 'rta') {
		const span = nearestOption('select-rta-span', w.hi - w.lo) ?? (w.hi - w.lo);
		const n = normalizeCenterSpan(center, span, S.FREQ_MIN, S.FREQ_MAX, 1000);
		if (!n) return null;
		selectOption('select-rta-span', span);
    const request_id = beginFrequencyCommit('rta-freq-settings', n);
    beginRtaAxisTransition(request_id);
    send({ cmd: 'SET_RTA', center: n.center, span: n.span, request_id });
		return { lo: n.start, hi: n.stop };
	}
	// SDR widths are fixed IQ steps. The backend confirms the actual center and listen
	// after fitting the capture and its occupied channel to the hardware passband.
	const decimate = nearestDecimate(w.hi - w.lo);
	if (decimate === null) return null;
	const span = estimatedCaptureSpanHz(decimate);
	const fitted = clampFreq({ lo: center - span / 2, hi: center + span / 2 });
	selectOption('select-sdr-decimate', decimate);
	cancelSdrTuneRequest();
	if (sdrInFlight) sdrQueued = fitted;
	else submitSdrWindow(fitted);
	return fitted;
}

/** One device reconfiguration at a time, with only the latest unsent window retained. */
function submitSdrWindow(freq: FreqWindow): void {
	const decimate = nearestDecimate(freq.hi - freq.lo);
	if (decimate === null) return;
	cancelSdrTuneRequest();
	const id = `sdr-axis-${++sdrSerial}`;
	sdrInFlight = { id, freq, revision: S.configVersion, timer: window.setTimeout(() => {
		if (sdrInFlight?.id !== id) return;
		// A held drag may release the discarded target later; it is no longer submitted.
		if (session?.axis === 'x' && session.settle?.freq === sdrQueued) session.settle = null;
		cancelSdrWindow();
		if (session?.axis === 'x' && !session.down && !session.live) clearPreview();
		if (chain?.axis === 'x') chain = null;
	}, SETTLE_MAX_MS) };
	send({ cmd: 'SET_SDR', center: (freq.lo + freq.hi) / 2, decimate, request_id: id });
	noteCommit();
}
