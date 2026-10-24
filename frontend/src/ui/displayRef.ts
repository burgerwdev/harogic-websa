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
 *   1. matching report - a user Ref request is acknowledged only by the stored SDR hardware
 *                       request matching the newest intent; effective IQS Ref may be clamped.
 *   2. supersede        - a newer request replaces the previous one and its timeout.
 *   3. timeout notify   - an unconfirmed request restores the previous display and reports timeout.
 *   4. divergence shown - `displayRefDiverges()` exposes requested-vs-reported for the UI.
 *
 * In SDR the session owns the scale even with auto-scale off. Background 'mode' writes
 * cannot replace it; an explicit 'preset' resets both display and device reference.
 */
import { createParam } from '../core/params';
import { currentGraphMode } from './graphMode';

export type DisplayRefSource = 'auto' | 'user' | 'sdr' | 'mode' | 'preset';

/** How long a user Ref request may stay unacknowledged before we say so. */
export const DISPLAY_REF_TTL_MS = 5000;

// Client-owned: nothing confirms a *display* scale (the backend reports the hardware Ref,
// which is a different value in SDR), so the value must not expire.
const value = createParam<number>('display.ref', {
	fallback: 0,
	scope: 'display',
	authoritative: true,
	equals: (a, b) => Math.abs(a - b) < 0.05,
});

/** Last confirmed hardware request, independently of the IQS effective Ref. */
let reported: number | null = null;
let requestSeq = 0;
let pending: { id: number; want: number; rollback: number; timer: number } | null = null;

export type DisplayRefTimeoutHandler = (want: number, reported: number | null) => void;
let timeoutHandler: DisplayRefTimeoutHandler | null = null;

export function setDisplayRefTimeoutHandler(fn: DisplayRefTimeoutHandler | null): void {
	timeoutHandler = fn;
}

export function getDisplayRef(): number {
	return value.get();
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

/** The user request still waiting for a matching report, if any. */
export function pendingDisplayRef(): number | null {
	return pending ? pending.want : null;
}

/** Explicit Auto supersedes hardware intent without restoring the previous display. */
export function cancelDisplayRefRequest(): void {
	if (pending !== null) {
		window.clearTimeout(pending.timer);
		pending = null;
	}
}

/** Rejected hardware intent restores the display that was confirmed before the edit. */
export function rejectDisplayRefRequest(): boolean {
	if (!pending) return false;
	const rollback = pending.rollback;
	cancelDisplayRefRequest();
	value.set(rollback);
	return true;
}

/**
 * The single write entry point. Returns false when the source is not allowed to write (a
 * refused write is a normal outcome: a higher-priority owner is active).
 */
export function setDisplayRef(source: DisplayRefSource, v: number): boolean {
	if (source === 'mode' && currentGraphMode() === 'sdr') return false;
	if (source === 'user') {
		const rollback = pending?.rollback ?? value.get();
		cancelDisplayRefRequest();
		const id = ++requestSeq;
		const timer = window.setTimeout(() => {
			if (pending === null || pending.id !== id) return;
			rejectDisplayRefRequest();
			timeoutHandler?.(v, reported);
		}, DISPLAY_REF_TTL_MS);
		pending = { id, want: v, rollback, timer };
	}
	else if (source !== 'mode') cancelDisplayRefRequest();
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
