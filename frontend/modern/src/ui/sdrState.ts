/**
 * SDR tuning state - the single owner of every SDR parameter.
 *
 * Before this module the same facts lived in three places at once: module variables in
 * controls.ts (`sdrCenterHz`/`sdrSpanHz`/`sdrDecimate`), exported variables in core/store.ts
 * (`sdrListenHz`/`sdrPassbandHz`), and the form controls themselves - each written by a
 * different code path, so they could disagree (a stale hand-off survived a Preset and
 * reapplied the pre-reset frequency; the listen value in the store and in the input could
 * differ after a reload).
 *
 * Rules for this module:
 *   - only the STATUS handler calls `confirm()`
 *   - only user actions call `set()`
 *   - `renderSdrState()` is the only place that writes the SDR form controls
 *   - nothing else keeps a copy of these values; read them with `get()`
 */
import { createParam, resetAll } from '../core/params';

/** Frequency-like equality: sub-Hz differences are the same value. */
const hz = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.5,
};

/** Requested wideband centre. `set()` here is what the old `pendingSdrFreq` hand-off did. */
export const sdrCenterHz = createParam<number>('sdr.center', { fallback: 0, scope: 'sdr', ...hz });
/** Requested IQ decimation (capture bandwidth = 62.5 MHz / decimate). */
export const sdrDecimate = createParam<number>('sdr.decimate', { fallback: 32, scope: 'sdr', ...hz });
/** Actual capture span reported by the device (`stop - start`); confirmed only. */
export const sdrSpanHz = createParam<number>('sdr.span', { fallback: 0, scope: 'sdr', ...hz });
/** Demodulator listen frequency. */
export const sdrListenHz = createParam<number>('sdr.listen', { fallback: 0, scope: 'sdr', ...hz });
/** Drop every pending SDR intent (Preset, or leaving the mode). */
export function resetSdrState(): void {
	resetAll('sdr');
}

const input = (id: string) => document.getElementById(id) as HTMLInputElement | null;
const select = (id: string) => document.getElementById(id) as HTMLSelectElement | null;

/**
 * Write the SDR form controls from the slots. The only writer, so the inputs can never hold
 * a value the state does not have. A control the user is currently editing is left alone.
 */
export function renderSdrState(): void {
	const center = input('input-sdr-center');
	if (center && document.activeElement !== center && sdrCenterHz.get() > 0) {
		center.value = (sdrCenterHz.get() / 1e6).toFixed(6);
	}
	const listen = input('input-sdr-listen');
	if (listen && document.activeElement !== listen && sdrListenHz.get() > 0) {
		listen.value = (sdrListenHz.get() / 1e6).toFixed(6);
	}
	const dec = select('select-sdr-decimate');
	if (dec) dec.value = String(sdrDecimate.get());
	// The demod group (mode/IF bandwidth/de-emphasis) is not migrated yet and is still
	// rendered by syncSdrPanel; it moves here in the next step.
}
