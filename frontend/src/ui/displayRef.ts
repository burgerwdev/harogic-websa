/**
 * Display reference level - the top line of the graticule.
 *
 * Display-reference writes have explicit owners and follow the same acknowledgement,
 * supersession, timeout and divergence rules as the mode handshake (see ui/graphMode.ts):
 *
 *   owners:
 *     auto   - the SDR client-side auto-scale
 *     user   - the Ref Set button / arrows (an explicit takeover, acknowledged by the device)
 *     sdr    - restore the SDR display scale once on entry, without a hardware write
 *     mode   - display defaults (normalise sets 0, un-normalise restores the level)
 *     preset - global reset
 *
 *   1. matching report - the clamped raw hardware request acknowledges the local display target.
 *   2. supersede        - a newer hardware request replaces the previous one and its timeout.
 *   3. timeout notify   - an unconfirmed request restores the previous display and reports timeout.
 *   4. divergence shown - `displayRefDiverges()` exposes requested-vs-reported for the UI.
 *
 * In SDR the session owns the scale even with auto-scale off. Background 'mode' writes
 * cannot replace it; an explicit 'preset' resets both display and device reference.
 */
import { createParam } from '../core/params';
import { currentGraphMode } from './graphMode';
import { displayOffset } from './displayState';
import { dbPerDiv, totalDivs } from '../core/store';
import { clearLevelView, getLevelView, setLevelView, zoomEnabled } from './spectrumViewport';

export type DisplayRefSource = 'auto' | 'user' | 'sdr' | 'mode' | 'normalize' | 'preset';

/** How long a user Ref request may stay unacknowledged before we say so. */
export const DISPLAY_REF_TTL_MS = 5000;

// Client-owned: hardware reports acknowledge a request, never replace a manual viewport.
const value = createParam<number>('display.ref', {
	fallback: 0,
	scope: 'display',
	authoritative: true,
	equals: (a, b) => Math.abs(a - b) < 0.05,
});

/** Last confirmed hardware request, independently of the IQS effective Ref. */
let reported: number | null = null;
let requestSeq = 0;
let owner: { raw: number } | null = null;
let pending: { id: number; want: number; rollback: number; rollbackOwner: { raw: number } | null; timer: number } | null = null;

export type DisplayRefTimeoutHandler = (want: number, reported: number | null) => void;
let timeoutHandler: DisplayRefTimeoutHandler | null = null;

export function setDisplayRefTimeoutHandler(fn: DisplayRefTimeoutHandler | null): void {
	timeoutHandler = fn;
}

export type DisplayRefShiftHandler = (delta: number) => void;
let shiftHandler: DisplayRefShiftHandler | null = null;

/** Axis gestures subscribe so offset edits keep a held or pending drag on the same raw window. */
export function setDisplayRefShiftHandler(fn: DisplayRefShiftHandler | null): void {
	shiftHandler = fn;
}

export function getDisplayRef(): number {
	return value.get();
}

/** Zoom bounds are a separate raw-dBm viewport; receiver Ref and its ACK owner stay intact. */
export function hasDisplayBounds(): boolean { return zoomEnabled.get() && getLevelView() !== null; }
export function getDisplayTop(): number {
	return hasDisplayBounds() ? getLevelView()!.top + displayOffset.get() : getDisplayRef();
}
export function getDisplayRange(): number {
	const bounds = hasDisplayBounds() ? getLevelView() : null;
	return bounds ? bounds.top - bounds.bottom : totalDivs * dbPerDiv;
}
export function displayBoundsMin(): number { return rawToDisplayRef(-300); }
export function displayBoundsMax(): number { return rawToDisplayRef(100); }
export function setDisplayBounds(top: number, bottom: number): boolean {
	if (!Number.isFinite(top) || !Number.isFinite(bottom)) return false;
	const offset = displayOffset.get();
	return setLevelView(Math.min(100, Math.max(-300, top - offset)),
		Math.min(100, Math.max(-300, bottom - offset)));
}
export function clearDisplayBounds(): void { clearLevelView(); }

/** Absolute display references include external gain/loss; hardware references never do. */
export function rawToDisplayRef(raw: number): number {
	return raw + displayOffset.get();
}

export function displayToRawRef(display: number): number {
	return display - displayOffset.get();
}

/** Local viewport bounds in raw-equivalent dBm; Offset shifts the absolute display once. */
export function displayRefMin(): number { return rawToDisplayRef(-200); }
export function displayRefMax(): number { return rawToDisplayRef(100); }

/** The verified SET_REF protocol bounds remain independent of the viewport. */
export function hardwareRefForDisplay(display: number): number {
	return Math.max(-160, Math.min(40, displayToRawRef(display)));
}

/** Same clamped hardware intent can serve multiple viewport moves, including while pending. */
export function needsHardwareRef(display: number): boolean {
	const current = pending?.want ?? owner?.raw ?? reported;
	return current === null || Math.abs(hardwareRefForDisplay(display) - current) >= 0.05;
}

export function clampDisplayRef(display: number): number {
	return Math.max(displayRefMin(), Math.min(displayRefMax(), display));
}

/** Offset edits preserve hardware intent, the rollback window, and a subscribed axis gesture. */
export function shiftDisplayRef(delta: number): void {
	value.set(value.get() + delta);
	if (pending) pending.rollback += delta;
	shiftHandler?.(delta);
}

/** The device-reported level, for diagnostics and the divergence test. */
export function reportedDisplayRef(): number | null {
	return reported;
}

/** True while the newest user request has not been confirmed by the device (discipline 4). */
export function displayRefDiverges(): boolean {
	if (pending === null) return false;
	return reported === null || Math.abs(pending.want - reported) >= 0.05;
}

/** Compensated user request still waiting for a matching raw hardware report, if any. */
export function pendingDisplayRef(): number | null {
	return pending ? value.get() : null;
}

/** Explicit Auto supersedes hardware intent without restoring the previous display. */
export function cancelDisplayRefRequest(): void {
	if (pending !== null) {
		window.clearTimeout(pending.timer);
		pending = null;
	}
}

/** Auto, Preset and confirmed mode changes relinquish the manual viewport. */
export function releaseDisplayRef(): void {
	cancelDisplayRefRequest();
	owner = null;
}

export function resetDisplayRefMode(): void {
	releaseDisplayRef();
	reported = null;
}

/** Rejected hardware intent restores the display that was confirmed before the edit. */
export function rejectDisplayRefRequest(): boolean {
	if (!pending) return false;
	const rollback = pending.rollback;
	owner = pending.rollbackOwner;
	cancelDisplayRefRequest();
	value.set(rollback);
	return true;
}

/**
 * The single write entry point. Returns false when the source is not allowed to write (a
 * refused write is a normal outcome: a higher-priority owner is active).
 */
export function setDisplayRef(source: DisplayRefSource, v: number): boolean {
	if (source === 'normalize' && currentGraphMode() === 'sdr') return false;
	if (source === 'mode' && (currentGraphMode() === 'sdr' || owner !== null)) return false;
	if (source === 'user') {
		const raw = hardwareRefForDisplay(v);
		if (needsHardwareRef(v)) {
			const rollback = pending?.rollback ?? value.get();
			const rollbackOwner = pending ? pending.rollbackOwner : owner;
			cancelDisplayRefRequest();
			const id = ++requestSeq;
			const timer = window.setTimeout(() => {
				if (pending === null || pending.id !== id) return;
				const want = value.get();
				rejectDisplayRefRequest();
				timeoutHandler?.(want, reported);
			}, DISPLAY_REF_TTL_MS);
			pending = { id, want: raw, rollback, rollbackOwner, timer };
		}
		owner = { raw };
	} else if (source !== 'mode') {
		releaseDisplayRef();
		if (source === 'preset') reported = null;
	}
	value.set(v);
	return true;
}

/**
 * STATUS reported the stored hardware request. A matching newest intent acknowledges it;
 * a stale report leaves it pending until rejection or timeout.
 */
export function noteDisplayRefReport(v: number): void {
	if (!Number.isFinite(v)) return;
	reported = v;
	if (pending !== null && Math.abs(pending.want - v) < 0.5) cancelDisplayRefRequest();
}
