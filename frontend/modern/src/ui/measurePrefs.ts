/**
 * Measurement/UI preferences as slots (report finding P1-6, B4).
 *
 * These are client-owned: the backend never reports them, so they must be `authoritative`
 * (otherwise the intent TTL would revert them after a few seconds - a real defect when the
 * display unit was migrated). They used to be plain store globals, some written by direct
 * assignment from several places; every writer now goes through `set()`.
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
