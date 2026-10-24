/**
 * Swept (SWP) resolution/points state - one owner per parameter.
 *
 * Only STATUS confirms settings, only user actions set intent, and readers call get().
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
/** Per-bin inter-frame detector (Detector_TypeDef); 'device_default' leaves the profile field. */
export const frameDetector = createParam<string>('swp.frameDetector', { fallback: 'device_default', scope: 'swp' });
/** Trace alignment on the frequency axis (TraceAlign_TypeDef). */
export const traceAlign = createParam<string>('swp.traceAlign', { fallback: 'start', scope: 'swp' });
/** Trace-points mapping strategy (TracePointsStrategy_TypeDef). */
export const pointsStrategy = createParam<string>('swp.pointsStrategy', { fallback: 'speed', scope: 'swp' });

/** Span step used by the ▼/▲ buttons (Hz); Auto makes it follow the current span. */
export const spanStepHz = createParam<number>('swp.spanStepHz', {
	fallback: 10e6, scope: 'swp', authoritative: true, parse: Number, serialize: String,
	equals: (a, b) => Math.abs(a - b) < 0.5,
});
export const spanStepAuto = createParam<boolean>('swp.spanStepAuto', {
	fallback: true, scope: 'swp', authoritative: true,
});
