/**
 * Display reference level - the top line of the graticule.
 *
 * The value had nine writers in five files with no arbitration ("last write wins"), which is
 * how a Preset, a normalise toggle or a trace reset could silently clobber the Ref the user
 * had just set in SDR. Ownership is explicit now, and the same four disciplines as the mode
 * handshake apply (see ui/graphMode.ts):
 *
 *   owners:
 *     auto   - the SDR client-side auto-scale
 *     user   - the Ref Set button / arrows (an explicit takeover)
 *     mode   - display defaults (normalise sets 0, un-normalise restores the level)
 *     preset - global reset
 *
 *   1. id-matched ack   - a user Ref request is acknowledged only by a backend report that
 *                         matches the newest request; older reports are ignored.
 *   2. supersede        - a newer request replaces the previous one and its timeout.
 *   3. timeout notify   - if the device never reports the requested level, the user is told.
 *   4. divergence shown - `displayRefDiverges()` exposes requested-vs-reported for the UI.
 *
 * In SDR the scale belongs to the SDR session even when the auto-scale is off, so a
 * background 'mode' write (normalise / trace switch / trace reset) is refused there:
 * allowing it was the "manual Ref returns to 0" bug. An explicit 'preset' is still honoured
 * - the user asked for factory defaults, and the device resets its reference too.
 */
import { createParam } from '../core/params';
import { currentGraphMode } from './graphMode';

export type DisplayRefSource = 'auto' | 'user' | 'mode' | 'preset';

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

/** Last effective reference level the device reported. */
let reported: number | null = null;
let requestSeq = 0;
let pending: { id: number; want: number; timer: number } | null = null;

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

function clearPending(): void {
	if (pending !== null) {
		window.clearTimeout(pending.timer);
		pending = null;
	}
}

/**
 * The single write entry point. Returns false when the source is not allowed to write (a
 * refused write is a normal outcome: a higher-priority owner is active).
 */
export function setDisplayRef(source: DisplayRefSource, v: number): boolean {
	if (source === 'mode' && currentGraphMode() === 'sdr') return false;
	if (source === 'user') {
		// Supersede: the newest request wins and restarts the timeout (discipline 2).
		clearPending();
		const id = ++requestSeq;
		const timer = window.setTimeout(() => {
			if (pending === null || pending.id !== id) return;
			pending = null;
			timeoutHandler?.(v, reported);
		}, DISPLAY_REF_TTL_MS);
		pending = { id, want: v, timer };
	}
	value.set(v);
	return true;
}

/**
 * The device reported its effective reference level (STATUS). A report matching the newest
 * user request acknowledges it (discipline 1); a report that does not match leaves the
 * request pending until its timeout.
 */
export function noteDisplayRefReport(v: number): void {
	if (!Number.isFinite(v)) return;
	reported = v;
	if (pending !== null && Math.abs(pending.want - v) < 0.5) clearPending();
}
