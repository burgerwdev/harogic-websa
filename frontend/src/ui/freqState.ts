/**
 * Frequency-window state for the swept (SWP) and real-time (RTA) modes.
 *
 * One owner per parameter, the same rule as the SDR group: only the STATUS handler calls
 * confirm(), only user actions call set(), every reader goes through get().
 *
 * Three windows exist and must not be confused (this is what `swpCenterHz` was added for):
 *   centerHz    - the active hardware window's centre; the STATUS `center` field is
 *                 mode-dependent, so in SDR it is the SDR centre
 *   swpCenterHz - the last centre confirmed by a SWP-family STATUS, i.e. the swept view's
 *                 centre, which is what an SDR hand-off must use
 *   rtaCenterHz - the RTA window's centre while an RTA STATUS is active
 */
import { createParam } from '../core/params';

const hz = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.5,
};

/** Centre of the active hardware window (mode-dependent meaning on the wire). */
export const centerHz = createParam<number>('freq.center', { fallback: 1e9, scope: 'swp', ...hz });
/** Span of the active hardware window. */
export const spanHz = createParam<number>('freq.span', { fallback: 100e6, scope: 'swp', ...hz });
/** Last centre confirmed while an SWP-family mode was active (never zero). */
export const swpCenterHz = createParam<number>('freq.swpCenter', { fallback: 0, scope: 'swp', ...hz });
/** Centre of the RTA window. */
export const rtaCenterHz = createParam<number>('freq.rtaCenter', { fallback: 1e9, scope: 'rta', ...hz });
