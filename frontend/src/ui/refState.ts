/**
 * Reference-level state for the swept (SWP) and real-time (RTA) modes.
 *
 * STATUS confirms the effective reference for the active hardware mode, user actions set
 * intent, and readers call get(). The slot owns pending intent and its expiry.
 */
import { createParam } from '../core/params';

const dB = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.05,
};

/** Reference level in dBm for the swept / real-time display (also the hardware ref). */
export const refLevel = createParam<number>('ref.level', { fallback: 0, scope: 'ref', ...dB });

let confirmedMode: string | null = null;

/** A mode handoff or command acknowledgement ends the previous mode's intent. */
export function confirmRefLevel(mode: string, value: unknown, acknowledgedRequest?: unknown): void {
	if (typeof value !== 'number' || !Number.isFinite(value)) return;
	const matchesIntent = refLevel.desiredValue() !== null && acknowledgedRequest === refLevel.desiredValue();
	if (mode !== confirmedMode || matchesIntent) refLevel.reset();
	confirmedMode = mode;
	refLevel.confirm(value);
}
