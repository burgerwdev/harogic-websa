/**
 * Waterfall persistence parameters as slots (report finding P1-6).
 *
 * These were plain store globals with hand-written setters, written by both the waterfall
 * panel and the localStorage restore path. They are user parameters, so they own slots:
 * `set()` on a user action, `confirm()` when the stored value is restored.
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
/** Per-frame density decay (persistence). */
export const rtaFade = createParam<number>('rta.fade', { fallback: 0.98, scope: 'wf', authoritative: true, ...num(1, 6) });
/** Probability-density amplitude bins (persisted). */
export const rtaAmpBins = createParam<number>('rta.ampBins', { fallback: 128, scope: 'wf', authoritative: true });
