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
/** Demodulation mode (am/fm/nfm/wfm/usb/lsb/cw). */
export const sdrDemod = createParam<string>('sdr.demod', { fallback: 'am', scope: 'sdr' });
/** IF passband in Hz (also what the listen-band overlay draws). */
export const sdrIfbw = createParam<number>('sdr.ifbw', { fallback: 6000, scope: 'sdr', ...hz });
/** FM de-emphasis time constant in microseconds (-1 = per-mode default). */
export const sdrDeemph = createParam<number>('sdr.deemph', { fallback: -1, scope: 'sdr', ...hz });

/**
 * Client-side preferences the backend does not report. Persisted here (the slot's own
 * single writer) instead of by whichever UI path happened to remember to write it.
 */
const flag = {
	parse: (raw: string) => raw === '1',
	serialize: (v: boolean) => (v ? '1' : '0'),
};
export const sdrRefAuto = createParam<boolean>('sdr.refAuto', {
	fallback: true, scope: 'sdr', persistKey: 'web-sa-sdr-ref-auto', persist: 'desired',
	authoritative: true, ...flag,
});
export const sdrAudioOn = createParam<boolean>('sdr.audioOn', {
	fallback: false, scope: 'sdr', persistKey: 'web-sa-sdr-audio', persist: 'desired',
	authoritative: true, ...flag,
});

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
	const demod = select('select-sdr-demod');
	if (demod) demod.value = sdrDemod.get();
	const ifbw = select('select-sdr-ifbw');
	if (ifbw) ifbw.value = String(Math.round(sdrIfbw.get()));
	const deemph = select('select-sdr-deemph');
	if (deemph) deemph.value = String(sdrDeemph.get());
}
