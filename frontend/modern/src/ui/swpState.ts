/**
 * Swept (SWP) resolution/points state - one owner per parameter.
 *
 * The same rule as the SDR and frequency groups: only the STATUS handler calls confirm(),
 * only user actions call set(), and every reader goes through get(). These values used to be
 * exported mutable `let`s in core/store.ts written by the STATUS handler and by two UI
 * functions; that made it impossible to tell an in-flight user choice from a confirmed
 * value, which is how "the value jumps back for a frame after I change it" happens.
 */
import { createParam } from '../core/params';

const hz = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.5,
};

/** RBW selection mode ('auto' | 'manual'). */
export const rbwMode = createParam<string>('swp.rbwMode', { fallback: 'auto', scope: 'swp' });
/** VBW selection mode ('bypass' | 'equal' | 'manual'). */
export const vbwMode = createParam<string>('swp.vbwMode', { fallback: 'bypass', scope: 'swp' });
/** Effective resolution bandwidth reported by the device (Hz). */
export const currentRBW = createParam<number>('swp.rbw', { fallback: 300e3, scope: 'swp', ...hz });
/** Effective video bandwidth reported by the device (Hz). */
export const currentVBW = createParam<number>('swp.vbw', { fallback: 300e3, scope: 'swp', ...hz });
/** Number of sweep points the device actually produces. */
export const currentPoints = createParam<number>('swp.points', { fallback: 1001, scope: 'swp', ...hz });
/** Spur suppression mode. */
export const currentSpur = createParam<string>('swp.spur', { fallback: 'standard', scope: 'swp' });
