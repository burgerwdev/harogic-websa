/**
 * SDR tuning state - the single owner of every SDR parameter.
 *
 * Rules for this module:
 *   - only the STATUS handler calls `confirm()`
 *   - only user actions call `set()`
 *   - `renderSdrState()` is the only place that writes the SDR form controls
 *   - nothing else keeps a copy of these values; read them with `get()`
 */
import { createParam, resetAll } from '../core/params';
import { toUnit } from '../core/units';
import { t } from '../core/i18n';
import { isDigitalMode } from '../sdr/registry';
import { FT8_SEARCH_HIGH_HZ, FT8_SEARCH_LOW_HZ } from '../sdr/ft8Log';
import { sstvBandHz } from '../sdr/sstvLog';
import { drmStatus } from '../sdr/drmLog';

/** Frequency-like equality: sub-Hz differences are the same value. */
const hz = {
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.5,
};

// Persist confirmed SDR preferences across mode re-entry and reload. Preset clears them
// through resetAll('sdr').
/** Requested wideband centre. */
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
/** Demodulation mode id from the backend plugin registry. */
export const sdrDemod = createParam<string>('sdr.demod', {
	fallback: 'am', scope: 'sdr', persistKey: 'web-sa-sdr-demod',
});

// The legacy pane stored Auto and the viewed mode separately. Only Auto off made that
// viewed mode a decoder pin; an Auto-on history filter must never become receiver config.
if (typeof localStorage !== 'undefined') {
	try {
		const auto = localStorage.getItem('web-sa-sstv-auto');
		if (auto !== null && localStorage.getItem('web-sa-sdr-sstv-mode') === null) {
			const raw = localStorage.getItem('web-sa-sstv-mode');
			const pin = raw !== null && raw.trim() !== '' ? Number(raw) : NaN;
			localStorage.setItem('web-sa-sdr-sstv-mode', String(auto === '0' && Number.isInteger(pin) && pin >= 0 ? pin : -1));
		}
		localStorage.removeItem('web-sa-sstv-auto');
	} catch { /* storage disabled */ }
}

/** Primary SSTV decoder mode: -1 lets the VIS header choose, >=0 pins a mode index. */
export const sdrSstvMode = createParam<number>('sdr.sstvMode', {
	fallback: -1, scope: 'sdr', persistKey: 'web-sa-sdr-sstv-mode',
	parse: (raw) => {
		const mode = raw.trim() !== '' ? Number(raw) : NaN;
		return Number.isInteger(mode) && mode >= -1 ? mode : null;
	},
});
/** IF passband in Hz (also what the listen-band overlay draws). */
export const sdrIfbw = createParam<number>('sdr.ifbw', {
	fallback: 6000, scope: 'sdr', persistKey: 'web-sa-sdr-ifbw', ...hz,
});
/** Native CW pitch from STATUS; the panel has no pitch setting. */
export const sdrPitchHz = createParam<number>('sdr.pitch', { fallback: 700, scope: 'sdr', ...hz });

/**
 * The band the active demodulator actually reads, in Hz relative to the listen frequency.
 *
 * SSB and CW follow the native analog filter specification, including the carrier guard and
 * STATUS-reported CW pitch. Protocol overlays show the decoder's actual receive band.
 */
export function demodBandHz(demod: string, ifBw: number, pitch = sdrPitchHz.get()): [number, number] {
	if (demod === 'drmplus') return [-50_000, 50_000];
	if (demod === 'drm') {
		const locked = drmStatus().lines.find((line) => line.startsWith('locked: '));
		const bandwidth = locked?.match(/, ([\d.]+) kHz/);
		// Until the FAC selects the occupancy, show the full DRM30 search band.
		const halfWidth = bandwidth ? Number(bandwidth[1]) * 500 : 10_000;
		return [-halfWidth, halfWidth];
	}
	if (demod === 'sstv') return sstvBandHz();
	if (isDigitalMode(demod)) return [FT8_SEARCH_LOW_HZ, FT8_SEARCH_HIGH_HZ];
	const guard = Math.max(100, Math.min(300, ifBw * 0.1));
	if (demod === 'usb') return [guard, ifBw];
	if (demod === 'lsb') return [-ifBw, -guard];
	if (demod === 'cw') {
		const half = Math.max(50, ifBw) / 2;
		const center = Math.max(100, pitch);
		return [center - half, center + half];
	}
	return [-ifBw / 2, ifBw / 2];
}
/** FM de-emphasis time constant in microseconds (-1 = per-mode default). */
export const sdrDeemph = createParam<number>('sdr.deemph', {
	fallback: -1, scope: 'sdr', persistKey: 'web-sa-sdr-deemph', ...hz,
});
/** Audio volume (0..2). */
export const sdrVolume = createParam<number>('sdr.volume', {
	fallback: 0.8, scope: 'sdr', persistKey: 'web-sa-sdr-volume',
	// Volume uses a 0.01 equality tolerance; zero is a valid value.
	parse: Number,
	serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.01,
});
/** Squelch threshold in the SDK's tuned-bin dBm, before either audio AGC. */
export const sdrSquelch = createParam<number>('sdr.squelch', {
	fallback: -110, scope: 'sdr', persistKey: 'web-sa-sdr-squelch',
	parse: Number, serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.05,
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

/** AM synchronous detector; only STATUS confirmation persists the receiver preference. */
export const sdrAmSync = createParam<boolean>('sdr.amSync', {
	fallback: false, scope: 'sdr', persistKey: 'web-sa-sdr-am-sync', ...flag,
});

/** Receiver noise reduction; confirmed by STATUS alongside the other audio settings. */
export const sdrNr = createParam<boolean>('sdr.nr', {
    fallback: false, scope: 'sdr', persistKey: 'web-sa-sdr-nr', ...flag,
});
/** How hard the reducer pushes, 0.05..1 (the panel's strength control). */
export const sdrNrStrength = createParam<number>('sdr.nrStrength', {
    fallback: 0.6, scope: 'sdr', persistKey: 'web-sa-sdr-nr-strength',
    parse: Number, serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.005,
});

/** Native receiver noise reduction algorithm. */
export type NrAlgo = 'wiener' | 'dfn';
export const sdrNrAlgo = createParam<NrAlgo>('sdr.nrAlgo', {
    fallback: 'wiener', scope: 'sdr', persistKey: 'web-sa-sdr-nr-algo',
	parse: (raw: string) => (raw === 'dfn' || raw === 'dfn2' ? 'dfn' : 'wiener'), // 'dfn2' = old persisted key
	serialize: String,
	equals: (a: NrAlgo, b: NrAlgo) => a === b,
});

/** DeepFilterNet3's attenuation limit in dB (0 = full passthrough, higher = stronger denoising
 * but clean signals are attenuated more). */
export const sdrNrAtten = createParam<number>('sdr.nrAtten', {
    fallback: 6, scope: 'sdr', persistKey: 'web-sa-sdr-nr-atten',
    parse: Number, serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.5,
});

export interface SdrChannelStatus {
    channel: number;
    enabled: boolean;
    freq: number;
    mode: string;
    if_bw: number;
    sstv_mode?: number | null;
    am_sync?: boolean;
    am_sync_generation?: number;
    volume: number;
    agc?: boolean;
    squelch?: number;
    pitch?: number;
    deemph_us?: number;
    nr?: boolean;
    nr_strength?: number;
    nr_algorithm?: string;
    nr_atten_db?: number;
    stereo?: boolean;
    audio_rate?: number;
}

/**
 * Tuning step, one entry per capture bandwidth.
 *
 * The user tunes with fixed steps, and a step that suits a 50 MHz span is useless inside a
 * 25 kHz one. So the step is remembered per bandwidth (key = decimation, the select's own
 * value) and the client owns it: the backend never reports a step, so the slot is
 * authoritative and persists with the other SDR preferences. The display unit lives
 * separately, in the shared unit map (core/units.ts) under the key `sdr_step`, the same
 * toggle buttons as every other frequency box.
 */

/**
 * Parse a stored step map. Keys are decimations, values are steps in Hz. Any entry that is
 * not a positive finite number is dropped, and unreadable storage degrades to an empty map,
 * so a hand-edited or damaged value cannot put a broken step on the arrow keys.
 */
export function parseSdrStepMap(raw: string): Record<string, number> {
	try {
		const map = JSON.parse(raw) as Record<string, unknown>;
		const clean: Record<string, number> = {};
		for (const [key, value] of Object.entries(map)) {
			if (typeof value === 'number' && isFinite(value) && value > 0) clean[key] = value;
		}
		return clean;
	} catch {
		return {};
	}
}

export const sdrStepByBw = createParam<Record<string, number>>('sdr.stepByBw', {
	fallback: {}, scope: 'sdr', persistKey: 'web-sa-sdr-step-bw', persist: 'desired',
	authoritative: true,
	parse: parseSdrStepMap,
	serialize: (v) => JSON.stringify(v),
	equals: (a, b) => {
		const ka = Object.keys(a), kb = Object.keys(b);
		return ka.length === kb.length && ka.every((k) => a[k] === b[k]);
	},
});

/** Every persisted SDR preference (Preset removes them, so defaults really are defaults). */
export const SDR_PREF_KEYS = [
	'web-sa-sdr-audio', 'web-sa-sdr-center', 'web-sa-sdr-listen', 'web-sa-sdr-decimate',
	'web-sa-sdr-demod', 'web-sa-sdr-ifbw', 'web-sa-sdr-deemph', 'web-sa-sdr-volume',
	'web-sa-sdr-squelch', 'web-sa-sdr-squelch-mode', 'web-sa-sdr-agc', 'web-sa-sdr-nr',
	'web-sa-sdr-nr-strength', 'web-sa-sdr-nr-algo', 'web-sa-sdr-nr-atten',
	'web-sa-sdr-step-bw', 'web-sa-sdr-sstv-mode', 'web-sa-sdr-am-sync',
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
	sdrSstvMode.set(-1);
	sdrAmSync.set(false);
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

/** Quick-select steps offered next to the step box, in Hz: 1 Hz up to 100 MHz. */
export const SDR_STEP_QUICK_HZ = [
	1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000,
] as const;

/**
 * Default step for a capture bandwidth: a 1-2-5 value at about span / 100, rounded down.
 *
 * A 195 kHz span then keeps the 1 kHz step the keyboard always used, a 25 kHz span drops to
 * 100 Hz for SSB and CW work, and a 50 MHz span gives 500 kHz so a wideband sweep stays
 * quick to walk through. Round down, not to the nearest: a finer step never hides a signal.
 */
export function defaultSdrStepHz(bandwidthHz: number): number {
	if (!(bandwidthHz > 0) || !isFinite(bandwidthHz)) return 1000;
	const target = bandwidthHz / 100;
	const decade = Math.floor(Math.log10(target));
	if (decade < 0) return 1;
	for (const m of [5, 2, 1]) {
		if (m * 10 ** decade <= target) return m * 10 ** decade;
	}
	return 1;
}

/** The step the user stored for one bandwidth, or null when none was set (an auto value). */
export function storedSdrStepHz(decimate: number): number | null {
	const stored = sdrStepByBw.get()[String(Math.round(decimate))];
	return stored && stored > 0 ? stored : null;
}

/** The step the arrow keys use now: the user's choice for this bandwidth, else the default. */
export function currentSdrStepHz(): number {
	const stored = storedSdrStepHz(sdrDecimate.get());
	if (stored !== null) return stored;
	const span = sdrSpanHz.get() > 0
		? sdrSpanHz.get()
		: estimatedCaptureSpanHz(Math.max(1, Math.round(sdrDecimate.get())));
	return defaultSdrStepHz(span);
}

/** Record the user's step for the current bandwidth (a user action; never an auto choice). */
export function setSdrStepForCurrentBw(stepHz: number): void {
	if (!(stepHz > 0) || !isFinite(stepHz)) return;
	const key = String(Math.round(sdrDecimate.get()));
	sdrStepByBw.set({ ...sdrStepByBw.get(), [key]: stepHz });
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
	const squelch = input('input-sdr-squelch');
	if (squelch && document.activeElement !== squelch) squelch.value = String(sdrSquelch.get());
	const squelchLabel = document.getElementById('cur-sdr-squelch');
	if (squelchLabel) {
		const threshold = Number(squelch?.value ?? sdrSquelch.get());
		squelchLabel.textContent = threshold <= -160 ? t('off') : `${threshold.toFixed(1)} dBm`;
	}
	// The tuning step: the box shows the value in the chosen unit, a quick button marks the
	// step in use. Read from the slots on every render, so a bandwidth switch shows that
	// bandwidth's stored step (or its default) with no separate copy to keep in step.
	const stepInput = input('input-sdr-step');
	if (stepInput && document.activeElement !== stepInput) {
		// The box shows the step in the shared unit map's unit for `sdr_step`.
		stepInput.value = String(Number(toUnit(currentSdrStepHz(), 'sdr_step').toPrecision(6)));
	}
	const stepHz = currentSdrStepHz();
	document.querySelectorAll<HTMLButtonElement>('#sdr-step-quick [data-step-hz]').forEach((btn) => {
		btn.classList.toggle('active', Number(btn.dataset.stepHz) === stepHz);
	});
	const nrButton = document.getElementById('btn-sdr-nr');
	if (nrButton) {
		nrButton.classList.toggle('active', sdrNr.get());
		nrButton.textContent = sdrNr.get() ? 'On' : 'Off';
	}
	const nrAlgo = select('select-sdr-nr-algo');
	if (nrAlgo) nrAlgo.value = sdrNrAlgo.get();
    const dfnSelected = sdrNrAlgo.get() === 'dfn';
	// The DeepFilterNet3 attenuation slider is only meaningful for dfn (Wiener has its own strength).
	const attenRow = document.getElementById('nr-atten-row');
	const attenInput = input('input-sdr-nr-atten');
	if (attenRow) attenRow.style.display = dfnSelected ? '' : 'none';
	if (attenInput) {
		attenInput.value = String(sdrNrAtten.get());
		const attenLabel = document.getElementById('cur-nr-atten');
		if (attenLabel) attenLabel.textContent = `${sdrNrAtten.get()} dB`;
	}
	const nrStrength = select('select-sdr-nr-strength');
	if (nrStrength) {
        // DeepFilterNet3 uses the attenuation control in place of strength.
		const dfn = sdrNrAlgo.get() === 'dfn';
		nrStrength.hidden = dfn;
		nrStrength.disabled = dfn || sdrDemod.get() === 'drm' || sdrDemod.get() === 'drmplus';
		// The nearest preset: the slot holds a number (a custom answer is possible via storage).
		const value = sdrNrStrength.get();
		let nearest = '0.6';
		for (const option of Array.from(nrStrength.options)) {
			if (Math.abs(Number(option.value) - value) < Math.abs(Number(nearest) - value)) {
				nearest = option.value;
			}
		}
		nrStrength.value = nearest;
	}
}
