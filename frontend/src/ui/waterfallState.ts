/**
 * Waterfall user settings use slots: user actions call set(), and stored values restore
 * through confirm().
 *
 * Rows/pushes themselves are data and stay in core/store.
 */
import { createParam } from '../core/params';

const num = (a: number, b: number) => ({
	parse: Number,
	serialize: String,
	equals: (x: number, y: number) => Math.abs(x - y) < a * Math.pow(10, -b),
});

/** Master waterfall enable (persisted). */
export const waterfallOn = createParam<boolean>('wf.on', { fallback: false, scope: 'wf', authoritative: true });
/** Colour range mode: auto scales per frame, fixed maps absolute dBm. */
export const wfRangeMode = createParam<'auto' | 'fixed'>('wf.rangeMode', { fallback: 'auto', scope: 'wf', authoritative: true });
/** Fixed-range lower level (dBm). */
export const wfLoDbm = createParam<number>('wf.lo', { fallback: -110, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Fixed-range upper level (dBm). */
export const wfHiDbm = createParam<number>('wf.hi', { fallback: -30, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Freeze the waterfall rows (they stop scrolling). */
export const wfPaused = createParam<boolean>('wf.paused', { fallback: false, scope: 'wf', authoritative: true });
/** Density afterglow half-life in seconds; 0 = Off, the density layer is not drawn at all. */
export const rtaHalflife = createParam<number>('rta.halflife', { fallback: 0.5, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Density input: the processed active trace, or the received frames before any accumulator. */
export const rtaDensSource = createParam<'trace' | 'live'>('rta.densSource', { fallback: 'trace', scope: 'wf', authoritative: true });
/** Which layers are visible: both (default), density only, or trace only. Display-only. */
export const rtaDensLayers = createParam<'both' | 'density' | 'trace'>('rta.densLayers', { fallback: 'both', scope: 'wf', authoritative: true });
/** Density colour-contrast: 0.5 soft .. 2.0 hard. Display mapping only, keeps history. */
export const rtaDensContrast = createParam<number>('rta.densContrast', { fallback: 1, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Vertical rendering footprint in CSS pixels; does not change amplitude history. */
export const rtaDensWidth = createParam<number>('rta.densWidth', { fallback: 5, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Probability-density amplitude bins (persisted). */
export const rtaAmpBins = createParam<number>('rta.ampBins', { fallback: 128, scope: 'wf', authoritative: true });
