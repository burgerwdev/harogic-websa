/**
 * Client-owned measurement/UI preferences. The backend never reports them, so they are
 * authoritative and do not expire with the intent TTL. Writers use set().
 */
import { createParam } from '../core/params';

/** Harmonic table data source: live / peak-hold / average / frozen. */
export const harmValMode = createParam<string>('harm.valMode', {
	fallback: 'RT', scope: 'meas', authoritative: true,
});
/** Peak list table visible. */
export const peakListVisible = createParam<boolean>('peak.listOn', {
	fallback: false, scope: 'meas', authoritative: true,
});
/** Peak search threshold in dBm: the value every peak/marker path reads. */
export const peakThr = createParam<number>('peak.thr', {
	fallback: -80, scope: 'meas', authoritative: true, parse: Number, serialize: String,
	equals: (a, b) => Math.abs(a - b) < 0.05,
});
/** True once the user edited the peak threshold (locks the auto value). */
export const peakThrUserSet = createParam<boolean>('peak.thrUserSet', {
	fallback: false, scope: 'meas', authoritative: true,
});
/** Normalisation reference window chosen by the user (0 = automatic). */
export const normRefWinUser = createParam<number>('norm.refWinUser', {
	fallback: 0, scope: 'meas', authoritative: true, parse: Number, serialize: String,
});
/** Index into the valley list, used by the peak/valley navigation. */
export const valleySeqPos = createParam<number>('marker.valleySeqPos', {
	fallback: 0, scope: 'meas', authoritative: true, parse: Number, serialize: String,
});
