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
import { radiofaxBandHz } from '../sdr/radiofaxLog';
import { signalLocateSupported, stepTierDefaults } from './sdrTuning';

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
/** Radiofax acquisition and raster preferences. STATUS alone confirms and persists them. */
export type RadiofaxAcquisition = 'auto' | 'manual';
export type RadiofaxIoc = 'auto' | '288' | '576';
export type RadiofaxLpm = 'auto' | '60' | '90' | '120' | '180' | '240';
export type RadiofaxModulation = 'fm' | 'am';
export type RadiofaxPolarity = 'normal' | 'inverted';
export type RadiofaxClockRecovery = 'auto' | 'off';
export const sdrRadiofaxAcquisition = createParam<RadiofaxAcquisition>('sdr.radiofaxAcquisition', {
	fallback: 'auto', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-acquisition',
	parse: (raw) => raw === 'auto' || raw === 'manual' ? raw : null,
});
export const sdrRadiofaxIoc = createParam<RadiofaxIoc>('sdr.radiofaxIoc', {
	fallback: 'auto', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-ioc',
	parse: (raw) => raw === 'auto' || raw === '288' || raw === '576' ? raw : null,
});
export const sdrRadiofaxLpm = createParam<RadiofaxLpm>('sdr.radiofaxLpm', {
	fallback: 'auto', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-lpm',
	parse: (raw) => ['auto', '60', '90', '120', '180', '240'].includes(raw) ? raw as RadiofaxLpm : null,
});
export const sdrRadiofaxModulation = createParam<RadiofaxModulation>('sdr.radiofaxModulation', {
	fallback: 'fm', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-modulation',
	parse: (raw) => raw === 'fm' || raw === 'am' ? raw : null,
});
export const sdrRadiofaxPolarity = createParam<RadiofaxPolarity>('sdr.radiofaxPolarity', {
	fallback: 'normal', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-polarity',
	parse: (raw) => raw === 'normal' || raw === 'inverted' ? raw : null,
});
export const sdrRadiofaxClockRecovery = createParam<RadiofaxClockRecovery>('sdr.radiofaxClockRecovery', {
	fallback: 'auto', scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-clock-recovery',
	parse: (raw) => raw === 'auto' || raw === 'off' ? raw : null,
});
const finiteRadiofaxCorrection = (raw: string): number | null => {
	const value = Number(raw);
	return raw.trim() !== '' && Number.isFinite(value) ? value : null;
};
export const sdrRadiofaxPhasePixels = createParam<number>('sdr.radiofaxPhasePixels', {
	fallback: 0, scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-phase-pixels',
	parse: finiteRadiofaxCorrection, serialize: String,
});
export const sdrRadiofaxClockPpm = createParam<number>('sdr.radiofaxClockPpm', {
	fallback: 0, scope: 'sdr', persistKey: 'web-sa-sdr-radiofax-clock-ppm',
	parse: finiteRadiofaxCorrection, serialize: String,
});
/** IF passband in Hz (also what the listen-band overlay draws). */
export const sdrIfbw = createParam<number>('sdr.ifbw', {
	fallback: 6000, scope: 'sdr', persistKey: 'web-sa-sdr-ifbw', ...hz,
});
/** CW sidetone/passband centre (100–1600 Hz), confirmed by STATUS. */
export const sdrPitchHz = createParam<number>('sdr.pitch', { fallback: 700, scope: 'sdr', ...hz });

export interface SdrFilterLimits { mode: string; min_hz: number; max_hz: number | null }
/** Physical sampling/capture limits reported by the native channel planner. */
export const sdrFilterLimits = createParam<SdrFilterLimits | null>('sdr.filterLimits', {
	fallback: null, scope: 'sdr',
});


/**
 * Parse the per-mode confirmed Filter preferences. Positive finite values are retained here;
 * mode/DSP/rate-plan validity is checked when a mode asks for its value, because CW validity
 * depends on the backend-confirmed pitch and native admission remains dynamic.
 */
export function parseSdrFilterMap(raw: string): Record<string, number> {
	try {
		const map = JSON.parse(raw) as Record<string, unknown>;
		const clean: Record<string, number> = {};
		for (const [mode, value] of Object.entries(map)) {
			if (mode && typeof value === 'number' && Number.isFinite(value) && value > 0) clean[mode] = value;
		}
		return clean;
	} catch {
		return {};
	}
}

/** Last backend-confirmed Filter value for each mode; a client-owned preference map. */
export const sdrFilterByMode = createParam<Record<string, number>>('sdr.filterByMode', {
	fallback: {}, scope: 'sdr', persistKey: 'web-sa-sdr-filter-by-mode', persist: 'desired',
	authoritative: true,
	parse: parseSdrFilterMap,
	serialize: (v) => JSON.stringify(v),
	equals: (a, b) => {
		const ka = Object.keys(a), kb = Object.keys(b);
		return ka.length === kb.length && ka.every((mode) => a[mode] === b[mode]);
	},
});
/**
 * The band the active demodulator actually reads, in Hz relative to the listen frequency.
 *
 * SSB and CW follow the native analog filter specification, including the carrier guard and
 * STATUS-reported CW pitch. Protocol overlays show the decoder's actual receive band.
 */
export function demodBandHz(demod: string, ifBw: number, pitch = sdrPitchHz.get()): [number, number] {
	if (demod === 'drmplus' || demod === 'drm') {
		// The native DRM chains keep the configured channel symmetric around the dial. FAC
		// identifies occupancy; it neither widens this window nor supplies display carrier edges.
		return [-ifBw / 2, ifBw / 2];
	}
	if (demod === 'sstv') return sstvBandHz();
	if (demod === 'radiofax') return radiofaxBandHz();
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
    radiofax_acquisition?: RadiofaxAcquisition;
    radiofax_ioc?: RadiofaxIoc;
    radiofax_lpm?: RadiofaxLpm;
    radiofax_modulation?: RadiofaxModulation;
    radiofax_polarity?: RadiofaxPolarity;
    radiofax_clock_recovery?: RadiofaxClockRecovery;
    radiofax_phase_pixels?: number;
    radiofax_clock_ppm?: number;
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

/** Tuning tier: the step box writes `normal`; Shift/Ctrl held arrows use the others. */
export type TuneTier = 'normal' | 'coarse' | 'fine';

/**
 * Parse a stored per-mode tier map. Values below 1 Hz or not positive finite are dropped,
 * and unreadable storage degrades to an empty map, so damaged storage cannot put a broken
 * step on the arrow keys.
 */
export function parseSdrStepMap(raw: string): Record<string, number> {
	try {
		const map = JSON.parse(raw) as Record<string, unknown>;
		const clean: Record<string, number> = {};
		for (const [mode, value] of Object.entries(map)) {
			if (typeof value === 'number' && Number.isFinite(value) && value >= 1) clean[mode] = value;
		}
		return clean;
	} catch {
		return {};
	}
}

const stepMapEquals = (a: Record<string, number>, b: Record<string, number>): boolean => {
	const ka = Object.keys(a), kb = Object.keys(b);
	return ka.length === kb.length && ka.every((mode) => a[mode] === b[mode]);
};

const stepMapSlot = (name: string, persistKey: string) => createParam<Record<string, number>>(
	name, {
		fallback: {}, scope: 'sdr', persistKey, persist: 'desired', authoritative: true,
		parse: parseSdrStepMap, serialize: (v) => JSON.stringify(v), equals: stepMapEquals,
	});

/** Normal tier per demod mode (Hz); authoritative client preference. */
export const sdrNormalStepByMode = stepMapSlot('sdr.normalStep', 'web-sa-sdr-step-normal');
/** Coarse tier per demod mode (Hz); authoritative client preference. */
export const sdrCoarseStepByMode = stepMapSlot('sdr.coarseStep', 'web-sa-sdr-step-coarse');
/** Fine tier per demod mode (Hz); authoritative client preference. */
export const sdrFineStepByMode = stepMapSlot('sdr.fineStep', 'web-sa-sdr-step-fine');

/** Stored spacing must be a finite value of at least 1 Hz; anything else reads the default. */
const stepHz = (raw: string): number | null => {
	const value = Number(raw);
	return Number.isFinite(value) && value >= 1 ? value : null;
};

/** Stored anchors may be any finite value, including a fractional offset from zero. */
const anchorHz = (raw: string): number | null => {
	const value = Number(raw);
	return Number.isFinite(value) ? value : null;
};

/** Click positioning: raw click, nearest channel-grid point, or strongest nearby signal. */
export type LocateMode = 'free' | 'channel' | 'signal';

/** Which positioning the user wants; a kept preference, reset by Preset. */
export const sdrLocateMode = createParam<LocateMode>('sdr.locate', {
	fallback: 'free', scope: 'sdr', persistKey: 'web-sa-sdr-locate', persist: 'desired',
	authoritative: true,
	parse: (raw) => (raw === 'channel' || raw === 'signal' ? raw : 'free'),
	serialize: String,
});

/**
 * The locate mode usable now: `signal` falls back to `free` where the mode has no carrier
 * evidence, and the preference itself stays stored for the return to a supported mode.
 */
export function effectiveLocateMode(mode: string): LocateMode {
	return sdrLocateMode.get() === 'signal' && !signalLocateSupported(mode) ? 'free'
		: sdrLocateMode.get();
}

/** Channel-grid spacing in Hz; any value from 1 Hz up, fractional channel plans included. */
export const sdrChannelStepHz = createParam<number>('sdr.channelStep', {
	fallback: 25_000, scope: 'sdr', persistKey: 'web-sa-sdr-channel-step', persist: 'desired',
	authoritative: true, parse: stepHz, serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.01,
});

/** Channel-grid anchor in Hz; the uniform grid runs `anchor + n * spacing`. */
export const sdrChannelAnchorHz = createParam<number>('sdr.channelAnchor', {
	fallback: 0, scope: 'sdr', persistKey: 'web-sa-sdr-channel-anchor', persist: 'desired',
	authoritative: true, parse: anchorHz, serialize: String,
	equals: (a: number, b: number) => Math.abs(a - b) < 0.01,
});



/** Every persisted SDR preference (Preset removes them, so defaults really are defaults). */
export const SDR_PREF_KEYS = [
	'web-sa-sdr-audio', 'web-sa-sdr-center', 'web-sa-sdr-listen', 'web-sa-sdr-decimate',
	'web-sa-sdr-demod', 'web-sa-sdr-ifbw', 'web-sa-sdr-deemph', 'web-sa-sdr-volume',
	'web-sa-sdr-squelch', 'web-sa-sdr-squelch-mode', 'web-sa-sdr-agc', 'web-sa-sdr-nr',
	'web-sa-sdr-nr-strength', 'web-sa-sdr-nr-algo', 'web-sa-sdr-nr-atten',
	'web-sa-sdr-step-normal', 'web-sa-sdr-step-coarse', 'web-sa-sdr-step-fine',
	'web-sa-sdr-locate', 'web-sa-sdr-channel-step', 'web-sa-sdr-channel-anchor',
	'web-sa-sdr-sstv-mode', 'web-sa-sdr-am-sync', 'web-sa-sdr-filter-by-mode',
	'web-sa-sdr-radiofax-acquisition', 'web-sa-sdr-radiofax-ioc', 'web-sa-sdr-radiofax-lpm',
	'web-sa-sdr-radiofax-modulation', 'web-sa-sdr-radiofax-polarity', 'web-sa-sdr-radiofax-clock-recovery',
	'web-sa-sdr-radiofax-phase-pixels', 'web-sa-sdr-radiofax-clock-ppm',
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
	sdrFilterLimits.confirm(null);
	sdrFilterByMode.set({});
	sdrSstvMode.set(-1);
	sdrAmSync.set(false);
	sdrRadiofaxAcquisition.set('auto');
	sdrRadiofaxIoc.set('auto');
	sdrRadiofaxLpm.set('auto');
	sdrRadiofaxModulation.set('fm');
	sdrRadiofaxPolarity.set('normal');
	sdrRadiofaxClockRecovery.set('auto');
	sdrRadiofaxPhasePixels.set(0);
	sdrRadiofaxClockPpm.set(0);
	sdrNormalStepByMode.set({});
	sdrCoarseStepByMode.set({});
	sdrFineStepByMode.set({});
	sdrLocateMode.set('free');
	sdrChannelStepHz.set(25_000);
	sdrChannelAnchorHz.set(0);
	if (typeof localStorage === 'undefined') return;
	try {
		SDR_PREF_KEYS.forEach((key) => localStorage.removeItem(key));
		localStorage.removeItem('web-sa-sdr-step-bw');
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
 * The step a tier uses now: the user's stored value for this demod mode, else the mode's
 * fixed default. One caller per action: the tune keys and the step box.
 */
export function currentSdrStepHz(tier: TuneTier = 'normal'): number {
	const mode = sdrDemod.get();
	const slot = tier === 'coarse' ? sdrCoarseStepByMode
		: tier === 'fine' ? sdrFineStepByMode : sdrNormalStepByMode;
	const remembered = slot.get()[mode];
	if (remembered !== undefined && Number.isFinite(remembered) && remembered >= 1) return remembered;
	return stepTierDefaults(mode)[tier];
}

/** Record the user's step for one tier of the current demod mode. */
export function setSdrStepForCurrentMode(stepHz: number, tier: TuneTier = 'normal'): void {
	if (!Number.isFinite(stepHz) || stepHz < 1) return;
	const mode = sdrDemod.get();
	const slot = tier === 'coarse' ? sdrCoarseStepByMode
		: tier === 'fine' ? sdrFineStepByMode : sdrNormalStepByMode;
	slot.set({ ...slot.get(), [mode]: stepHz });
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
		if (threshold <= -160) squelchLabel.dataset.i18n = 'squelch_off';
		else delete squelchLabel.dataset.i18n;
		squelchLabel.textContent = threshold <= -160 ? t('squelch_off') : `${threshold.toFixed(1)} dBm`;
	}
	// The tuning step: the box shows the value in the chosen unit, a quick button marks the
	// step in use. Read from the slots on every render, so a mode switch shows that mode's
	// remembered step (or its default) with no separate copy to keep in step.
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
