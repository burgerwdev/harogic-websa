/**
 * Acquisition mode handshake.
 *
 * The backend owns the mode; the frontend may *ask* for another one and has to wait for a
 * STATUS that confirms it. This used to be a boolean + a target string + a confirmed string
 * + a window.setTimeout watchdog - four pieces of state for one parameter, each updated by a
 * different branch. A slot expresses the same thing directly: `desired` is the request,
 * `confirmed` is what the backend reports, and the TTL replaces the watchdog so a dropped
 * reply cannot leave the mode buttons disabled forever.
 *
 * `currentGraphMode()` deliberately returns the CONFIRMED mode, never the pending request:
 * the panels keep showing the old mode until the hardware really switched, which is what
 * every caller (rendering, tuning, audio hand-off) assumes.
 */
import { createParam } from '../core/params';

export type GraphMode = 'std' | 'rta' | 'sdr';

export function isGraphMode(v: string): v is GraphMode {
	return v === 'std' || v === 'rta' || v === 'sdr';
}

/** How long a mode request may stay unconfirmed before the UI stops waiting for it. */
export const GRAPH_MODE_TTL_MS = 8000;

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
