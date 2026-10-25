/**
 * Hardware reference-level state for SWP, RTA and SDR.
 *
 * STATUS confirms the stored request separately from the SDK-effective reference.
 * User actions store clamped raw hardware intent; ui/displayRef owns the independent viewport.
 */
import { createParam } from '../core/params';

const dB = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.05,
};

/** Requested raw reference in dBm, before compensation; SDK front-end reference may differ. */
export const refLevel = createParam<number>('ref.level', { fallback: 0, scope: 'ref', ...dB });
/** SDK-effective reference, used to explain hardware gain decisions. */
export const effectiveRefLevel = createParam<number>('ref.effective', { fallback: 0, scope: 'ref', ...dB });

let confirmedMode: string | null = null;

/** A mode handoff or command acknowledgement ends the previous mode's intent. */
export function confirmRefLevel(mode: string, value: unknown, acknowledgedRequest?: unknown, storedRequest?: unknown): void {
	if (typeof value !== 'number' || !Number.isFinite(value)) return;
	effectiveRefLevel.confirm(value);
	const requested = typeof storedRequest === 'number' && Number.isFinite(storedRequest) ? storedRequest : value;
	const matchesIntent = refLevel.desiredValue() !== null && acknowledgedRequest === refLevel.desiredValue();
	if (mode !== confirmedMode || matchesIntent) refLevel.reset();
	confirmedMode = mode;
	refLevel.confirm(requested);
}
