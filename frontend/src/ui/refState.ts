/**
 * Reference-level state for the swept (SWP) and real-time (RTA) modes.
 *
 * Same single-owner rule as the SDR group: only the STATUS handler calls confirm(), only user
 * actions call set(), and every reader goes through get(). The RTA ref is not a separate
 * parameter - the STATUS `ref` field is already the effective value for the active hardware
 * mode, which is why the old store's unused `rtaRefLevel`/`rtaRefMode` copies are gone.
 *
 * This replaces the hand-written `refPending` + `refPendingAt` pair in controls.ts: stepping
 * the reference used to keep its own pending value with a 2.5 s timeout, which is exactly the
 * `desired` + TTL the slot already provides.
 */
import { createParam } from '../core/params';

const dB = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.05,
};

/** Reference level in dBm for the swept / real-time display (also the hardware ref). */
export const refLevel = createParam<number>('ref.level', { fallback: 0, scope: 'ref', ...dB });
