/**
 * Amplitude chain state - attenuation / preamp / IF gain / gain strategy.
 *
 * One slot per field; only STATUS confirms, only the selectors set intent, and readers
 * render `get()` so a still-pending user intent cannot be visually reverted by a stale
 * status report. A commit sends only the field the user changed.
 */
import { prepareSdrAudioTransition } from '../audio/sdrAudio';
import { t } from '../core/i18n';
import { createParam, type Param } from '../core/params';
import { send } from '../core/wsSend';
import { currentGraphMode, measurementOwnsDevice } from './graphMode';

export type AmpField = 'atten' | 'preamp' | 'ifgain' | 'gain_strategy';

const numeric = { parse: Number, serialize: String };

function ampNumber(amp: Record<string, unknown>, key: string): number | null {
	const v = amp[key];
	return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

/** Manual attenuation request (-1 = Auto). */
export const ampAtten = createParam<number>('amp.atten', { fallback: -1, scope: 'amp', ...numeric });
/** Preamplifier request (0 = Auto, 1 = Off). */
export const ampPreamp = createParam<number>('amp.preamp', { fallback: 0, scope: 'amp', ...numeric });
/** IF gain stage request (0..3). */
export const ampIfgain = createParam<number>('amp.ifgain', { fallback: 2, scope: 'amp', ...numeric });
/** Gain strategy request (0 = Low Noise, 1 = High Linearity). */
export const ampGainStrategy = createParam<number>('amp.gain_strategy', { fallback: 0, scope: 'amp', ...numeric });

/** Control metadata and legal integer ranges; the select options are static in index.html. */
export const AMP_FIELDS: Record<AmpField, { id: string; min: number; max: number; slot: Param<number> }> = {
	atten: { id: 'select-atten', min: -1, max: 33, slot: ampAtten },
	preamp: { id: 'select-preamp', min: 0, max: 1, slot: ampPreamp },
	ifgain: { id: 'select-ifgain', min: 0, max: 3, slot: ampIfgain },
	gain_strategy: { id: 'select-gainstrategy', min: 0, max: 1, slot: ampGainStrategy },
};

/**
 * Commit one selector's value: validate the integer range, record the intent and send a
 * SET_AMP carrying only that field. Returns false (and sends nothing) for an illegal value.
 */
export function setAmpField(field: AmpField, element?: HTMLSelectElement | null): boolean {
	const meta = AMP_FIELDS[field];
	const el = element ?? (document.getElementById(meta.id) as HTMLSelectElement | null);
	if (!el || el.value.trim() === '' || measurementOwnsDevice()) return false;
	const v = Number(el.value);
	if (!Number.isInteger(v) || v < meta.min || v > meta.max) return false;
	meta.slot.set(v);
	syncAmpAvailability(measurementOwnsDevice());
	if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
	send({ cmd: 'SET_AMP', [field]: v });
	return true;
}

/**
 * Apply a STATUS amp block: confirm strictly numeric requested fields, then render each
 * selector from `get()` (a fresh intent beats a stale report until its TTL expires).
 * Actual readouts are only written for strictly finite numeric values in range; null and
 * strings never render. Attenuation options are static - nothing is added dynamically.
 */
export function syncAmpStatus(amp: unknown): void {
	if (!amp || typeof amp !== 'object') return;
	// STATUS.amp block: untrusted shape, every field is checked before use below.
	const a = amp as Record<string, unknown>;
	for (const field of Object.keys(AMP_FIELDS) as AmpField[]) {
		const meta = AMP_FIELDS[field];
		const v = ampNumber(a, field);
		if (v !== null && Number.isInteger(v) && v >= meta.min && v <= meta.max) meta.slot.confirm(v);
		const el = document.getElementById(AMP_FIELDS[field].id) as HTMLSelectElement | null;
		if (el && document.activeElement !== el) el.value = String(AMP_FIELDS[field].slot.get());
	}

	const attenActual = ampNumber(a, 'atten_actual');
	const ca = document.getElementById('cur-atten');
	if (ca) {
		ca.textContent = attenActual !== null && attenActual >= 0 && attenActual <= 33 ? attenActual + ' dB' : '';
	}
	const preampActual = ampNumber(a, 'preamp_actual');
	const cp = document.getElementById('cur-preamp');
	if (cp) {
		cp.textContent = preampActual === 0 ? t('auto') : preampActual === 1 ? t('off') : '';
	}
	const ifgainActual = ampNumber(a, 'ifgain_actual');
	const cg = document.getElementById('cur-ifgain');
	if (cg) {
		cg.textContent = ifgainActual !== null && Number.isInteger(ifgainActual)
			&& ifgainActual >= 0 && ifgainActual <= 3 ? 'L' + ifgainActual : '';
	}
	syncAmpAvailability(measurementOwnsDevice());
}

/**
 * Selector availability: manual attenuation makes the device choose the preamplifier, and
 * an exclusive session owns the device entirely. The slot's desired value decides "manual"
 * so a just-made request disables the preamp selector before the status catches up.
 */
export function syncAmpAvailability(exclusive: boolean): void {
	const manualAtten = ampAtten.get() !== -1;
	for (const field of Object.keys(AMP_FIELDS) as AmpField[]) {
		const el = document.getElementById(AMP_FIELDS[field].id) as HTMLSelectElement | null;
		if (!el) continue;
		if (field === 'preamp') {
			el.disabled = manualAtten || exclusive;
			el.title = manualAtten ? t('preamp_manual_atten') : t('tip_select-preamp');
		} else {
			el.disabled = exclusive;
		}
	}
}
