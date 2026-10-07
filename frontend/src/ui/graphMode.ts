/**
 * Acquisition-mode handshake.
 *
 * The backend owns the mode; the front end may *ask* for another one and must wait for a
 * STATUS that reports it. The previous attempt at this slice was reverted because a pending
 * request blocked new ones (the "switching is slow / needs a second click" symptom). It is
 * rebuilt here against four disciplines:
 *
 *   1. id-matched ack   - only a STATUS that reports the newest requested mode is accepted;
 *                         an older reply (or a STATUS still on the old mode) is ignored.
 *   2. supersede        - a new request replaces the previous one immediately. The buttons
 *                         stay enabled, so a click while a switch is in flight is never lost.
 *   3. timeout notify   - if no confirming STATUS arrives within the TTL, the pending
 *                         request is dropped AND the user is told (never silently).
 *   4. divergence shown - while the request is unconfirmed the mode buttons are marked
 *                         "pending" and the hint names the mode being switched to.
 *
 * `currentGraphMode()` always returns the CONFIRMED mode: panels keep rendering the old mode
 * until the hardware really switched, which is what the renderer and the audio hand-off
 * assume.
 */
import { createParam } from '../core/params';

export type GraphMode = 'std' | 'rta' | 'sdr';

export function isGraphMode(v: string): v is GraphMode {
	return v === 'std' || v === 'rta' || v === 'sdr';
}

/** How long a mode request may stay unconfirmed before the UI stops waiting for it. */
export const GRAPH_MODE_TTL_MS = 8000;

/**
 * The mode the DEVICE reports, verbatim (`STATUS.mode`), including the measurement sessions.
 *
 * `graphMode` deliberately only knows the three view modes, so 'harmonic' / 'pnm' - which own the
 * device while they run - were dropped on the floor: the client could not tell that a measurement
 * was active unless IT had started one, and a measurement started elsewhere (a script, another
 * client, a restored session) left every mode-gated behaviour believing the swept/RTA view owned the
 * device. Measured: entering the harmonic mode through the API and dragging the frequency axis then
 * produced one alert per command, because the gesture sent SWP-owned commands that the command layer
 * refuses while a measurement owns the device.
 */
export const deviceMode = createParam<string>('device.mode', { fallback: 'std', scope: 'mode' });

/** True while a measurement session owns the device (SWP-owned commands are refused then). */
export function measurementOwnsDevice(): boolean {
	const mode = deviceMode.get();
	return mode === 'harmonic' || mode === 'pnm';
}

export const graphMode = createParam<GraphMode>('graph.mode', {
	fallback: 'std',
	scope: 'mode',
	ttlMs: GRAPH_MODE_TTL_MS,
});

/** The mode the backend reported (what the UI renders). */
export function currentGraphMode(): GraphMode {
	return graphMode.confirmedValue() ?? 'std';
}

/** The mode the user asked for, while it is still unconfirmed. */
export function pendingGraphMode(): GraphMode | null {
	return graphMode.pending() ? (graphMode.desiredValue() as GraphMode) : null;
}

/** True while the requested mode differs from the confirmed one (discipline 4). */
export function graphModeDiverges(): boolean {
	const want = pendingGraphMode();
	return want !== null && want !== currentGraphMode();
}

/** Called when a request expires unconfirmed (discipline 3). Wired to the UI by controls.ts. */
export type GraphModeTimeoutHandler = (want: GraphMode, have: GraphMode) => void;
let timeoutHandler: GraphModeTimeoutHandler | null = null;

export function setGraphModeTimeoutHandler(fn: GraphModeTimeoutHandler | null): void {
	timeoutHandler = fn;
}

let watchdog: number | null = null;

function clearWatchdog(): void {
	if (watchdog !== null) {
		window.clearTimeout(watchdog);
		watchdog = null;
	}
}

/**
 * Record a request. A newer request replaces the previous one (discipline 2) and restarts
 * the timeout; asking for the mode we are already in is a no-op.
 */
export function requestGraphMode(target: GraphMode): void {
	if (pendingGraphMode() === null && target === currentGraphMode()) return;
	graphMode.set(target);
	clearWatchdog();
	const id = graphMode.epoch;
	watchdog = window.setTimeout(() => {
		watchdog = null;
		if (graphMode.epoch !== id || !graphMode.pending()) return;   // superseded or confirmed
		const have = currentGraphMode();
		graphMode.reset();                                            // stop waiting, follow the backend
		timeoutHandler?.(target, have);
	}, GRAPH_MODE_TTL_MS);
}

/**
 * Apply a STATUS mode (discipline 1). Returns true when the confirmed mode changed.
 */
export function confirmGraphMode(mode: GraphMode): boolean {
	const want = pendingGraphMode();
	if (want !== null && mode !== want) return false;   // an older reply: not our answer yet
	const changed = graphMode.confirm(mode);
	if (want !== null && mode === want) clearWatchdog();
	return changed;
}

/** Drop a pending request (Preset / teardown). */
export function resetGraphMode(): void {
	clearWatchdog();
	graphMode.reset();
}
