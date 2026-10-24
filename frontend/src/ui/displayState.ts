/**
 * Display preferences use slots: set on a user action, confirm when derived from another setting.
 */
import { createParam } from '../core/params';

/** What the reference readout shows: dBm (absolute) or dB (relative to the ref level). */
export const displayUnit = createParam<'dBm' | 'dB'>('display.unit', {
	fallback: 'dBm', scope: 'display', authoritative: true,
});
/** External gain/loss compensation applied to every level readout (dB). */
export const displayOffset = createParam<number>('display.offset', {
	fallback: 0,
	scope: 'display',
	authoritative: true,
	parse: Number,
	serialize: String,
	equals: (a, b) => Math.abs(a - b) < 1e-6,
});
/** Savitzky-Golay display smoothing window in bins (1 = off). */
export const smoothBins = createParam<number>('display.smooth', {
	fallback: 1, scope: 'display', authoritative: true,
});
