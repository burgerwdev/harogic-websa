/**
 * Trigger parameters as slots (report finding P1-6).
 *
 * These four values used to be plain globals in core/store with hand-written setters, which
 * is the "several writers, no owner" pattern the parameter slots exist to remove: the STATUS
 * poll, the arm/free buttons and the canvas all wrote them. Ownership now follows the SDR
 * group's rule - only a backend report confirms, only a user action sets, readers call get().
 *
 * Runtime state that is not a parameter (armed/waiting/hit/overlay, the previous sweep for
 * the software trigger) stays in core/store.
 */
import { createParam } from '../core/params';

const dB = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 1e-9,
};

export type TriggerEdge = 'rising' | 'falling' | 'double';

/** Device trigger source (bus = free running host bus trigger, level = threshold). */
export const trigSource = createParam<string>('trigger.source', { fallback: 'bus', scope: 'trigger' });
/** Level-trigger threshold, dBm. */
export const trigLevel = createParam<number>('trigger.level', { fallback: -40.0, scope: 'trigger', ...dB });
/** Trigger edge; the swept software trigger uses the same setting. */
export const trigEdge = createParam<TriggerEdge>('trigger.edge', { fallback: 'rising', scope: 'trigger' });
/** Points of intercept reported by the device (read-only display value). */
export const trigPoi = createParam<number>('trigger.poi', { fallback: 0, scope: 'trigger', ...dB });

/** Apply one RTA STATUS block (req.rta) to the slots. */
export function confirmTriggerFromStatus(req: Record<string, any>): void {
	if (req.trigger_source !== undefined) trigSource.confirm(String(req.trigger_source));
	if (req.trigger_edge !== undefined) trigEdge.confirm(String(req.trigger_edge) as TriggerEdge);
	if (req.trigger_level !== undefined && Number.isFinite(Number(req.trigger_level))) {
		trigLevel.confirm(Number(req.trigger_level));
	}
	const actual = (req.trigger_actual ?? {}) as Record<string, any>;
	if (actual.poi !== undefined && Number.isFinite(Number(actual.poi))) {
		trigPoi.confirm(Number(actual.poi));
	}
}
