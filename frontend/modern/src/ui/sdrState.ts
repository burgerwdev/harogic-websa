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

// Every SDR setting the backend confirms is also the user's preference: it is persisted
// (`persist: 'confirmed'` stores what the device accepted) and re-applied when SDR is entered, so
// leaving the mode and coming back - or reloading the page - does not silently discard the tuning
// and the listening setup (reported: SDR settings were re-derived from the swept centre on every
// entry). `resetAll('sdr')` (Preset) drops the stored values too, so factory defaults really are
// defaults.
/** Requested wideband centre. `set()` here is what the old `pendingSdrFreq` hand-off did. */
export const sdrCenterHz = createParam<number>('sdr.center', {
	fallback: 0, scope: 'sdr', persistKey: 'web-sa-sdr-center', ...hz,
});
/** Requested IQ decimation (capture bandwidth = 62.5 MHz / decimate). */
export const sdrDecimate = createParam<number>('sdr.decimate', {
	fallback: 32, scope: 'sdr', persistKey: 'web-sa-sdr-decimate', ...hz,
});
/** Actual capture span reported by the device (`stop - start`); confirmed only. */
export const sdrSpanHz = createParam<number>('sdr.span', { fallback: 0, scope: 'sdr', ...hz });
/** Demodulator listen frequency. */
export const sdrListenHz = createParam<number>('sdr.listen', {
	fallback: 0, scope: 'sdr', persistKey: 'web-sa-sdr-listen', ...hz,
});
/** Demodulation mode (am/fm/nfm/wfm/usb/lsb/cw). */
export const sdrDemod = createParam<string>('sdr.demod', {
	fallback: 'am', scope: 'sdr', persistKey: 'web-sa-sdr-demod',
});
/** IF passband in Hz (also what the listen-band overlay draws). */
export const sdrIfbw = createParam<number>('sdr.ifbw', {
	fallback: 6000, scope: 'sdr', persistKey: 'web-sa-sdr-ifbw', ...hz,
});
/** FM de-emphasis time constant in microseconds (-1 = per-mode default). */
export const sdrDeemph = createParam<number>('sdr.deemph', {
	fallback: -1, scope: 'sdr', persistKey: 'web-sa-sdr-deemph', ...hz,
});
/** Audio volume (0..2). */
export const sdrVolume = createParam<number>('sdr.volume', {
	fallback: 0.8, scope: 'sdr', persistKey: 'web-sa-sdr-volume', ...hz,
});
/** Squelch threshold in dBFS. */
export const sdrSquelch = createParam<number>('sdr.squelch', {
	fallback: -110, scope: 'sdr', persistKey: 'web-sa-sdr-squelch', ...hz,
});
/** Audio AGC. */
export const sdrAgc = createParam<boolean>('sdr.agc', {
	fallback: true, scope: 'sdr', persistKey: 'web-sa-sdr-agc',
	parse: (raw: string) => raw === '1', serialize: (v: boolean) => (v ? '1' : '0'),
});

/**
 * Client-side preferences the backend does not report. Persisted here (the slot's own
 * single writer) instead of by whichever UI path happened to remember to write it.
 */
const flag = {
	parse: (raw: string) => raw === '1',
	serialize: (v: boolean) => (v ? '1' : '0'),
};
export const sdrAudioOn = createParam<boolean>('sdr.audioOn', {
	fallback: false, scope: 'sdr', persistKey: 'web-sa-sdr-audio', persist: 'desired',
	authoritative: true, ...flag,
});

/** Every persisted SDR preference (Preset removes them, so defaults really are defaults). */
export const SDR_PREF_KEYS = [
	'web-sa-sdr-audio', 'web-sa-sdr-center', 'web-sa-sdr-listen', 'web-sa-sdr-decimate',
	'web-sa-sdr-demod', 'web-sa-sdr-ifbw', 'web-sa-sdr-deemph', 'web-sa-sdr-volume',
	'web-sa-sdr-squelch', 'web-sa-sdr-agc',
];

/**
 * True when the user has an SDR preference stored (i.e. SDR has been set up before).
 *
 * The swept-centre hand-off and the band-derived demod are first-run conveniences: once the user
 * has their own setup, that setup wins (a mode switch is not a reset).
 */
export function hasStoredSdrPrefs(): boolean {
	if (typeof localStorage === 'undefined') return false;
	try {
		return SDR_PREF_KEYS.some((key) => localStorage.getItem(key) !== null);
	} catch {
		return false;
	}
}

/** Drop every pending SDR intent AND the stored preferences (Preset). */
export function resetSdrState(): void {
	resetAll('sdr');
	if (typeof localStorage === 'undefined') return;
	try {
		SDR_PREF_KEYS.forEach((key) => localStorage.removeItem(key));
	} catch {
		/* storage disabled: nothing to clear */
	}
}

/**
 * IQS native sample rate of the SAN series. The backend reads the real value from the
 * device profile (`NativeIQSampleRate_SPS`); this is only used to show a plausible capture
 * span for the few hundred ms before the first SDR STATUS arrives, and STATUS is the
 * authority (it overwrites the estimate with the measured bandwidth).
 */
export const IQS_NATIVE_RATE_HZ = 62.5e6;

/** Capture bandwidth estimate: 0.8 * native / decimate (the backend's own formula). */
export function estimatedCaptureSpanHz(decimate: number): number {
	return (IQS_NATIVE_RATE_HZ * 0.8) / Math.max(1, decimate);
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
}
