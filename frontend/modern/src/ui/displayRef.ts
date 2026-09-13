/**
 * Display reference level - the top of the graticule.
 *
 * This value had nine writers in five files with no arbitration ("last write wins"), which
 * is how a preset, a normalise toggle or a trace switch could silently clobber the SDR
 * auto-scale - the auto-scale then needed a self-healing comparison to notice. Ownership is
 * now explicit:
 *
 *   auto   - the SDR client-side auto-scale
 *   user   - the Ref Set button / arrows (they switch the auto-scale off first, so a
 *            takeover is explicit, never a silent fight)
 *   mode   - display-mode defaults: normalising sets 0, un-normalising restores the level
 *   preset - global reset
 *
 * While the SDR auto-scale is enabled it is the sole authority: `mode`/`preset` requests are
 * ignored so the two cannot fight. In SWP/RTA nothing owns it, so those defaults apply
 * normally (the auto-scale only ever runs in SDR).
 */
import { createParam } from '../core/params';
import { currentGraphMode } from './graphMode';
import { sdrRefAuto } from './sdrState';

export type DisplayRefSource = 'auto' | 'user' | 'mode' | 'preset';

const value = createParam<number>('display.ref', {
	fallback: 0,
	scope: 'display',
	parse: Number,
	serialize: String,
	equals: (a, b) => Math.abs(a - b) < 0.05,
});

/** True while the SDR auto-scale owns the display reference. */
export function autoScaleOwnsDisplayRef(): boolean {
	return currentGraphMode() === 'sdr' && sdrRefAuto.get();
}

export function getDisplayRef(): number {
	return value.get();
}

/** The single write entry point. Returns true when the value was applied. */
export function setDisplayRef(source: DisplayRefSource, v: number): boolean {
	if (source !== 'auto' && source !== 'user' && autoScaleOwnsDisplayRef()) return false;
	value.set(v);
	return true;
}

/** For diagnostics/tests: the value the backend-independent display state holds. */
export function displayRefOverride(): number | null {
	return value.desiredValue();
}
