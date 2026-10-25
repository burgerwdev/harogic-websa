/**
 * Amplitude chain state - attenuation / preamp / IF gain / gain strategy.
 *
 * One slot per requested field; correlated STATUS replies settle selector intent.
 * SDK actual values are readouts only. A commit sends only the field the user changed.
 */
import { prepareSdrAudioTransition } from '../audio/sdrAudio';
import { t } from '../core/i18n';
import { createParam, type Param } from '../core/params';
import { send } from '../core/wsSend';
import { currentGraphMode, measurementOwnsDevice } from './graphMode';

export type AmpField = 'atten' | 'preamp' | 'ifgain' | 'gain_strategy';

// The request watchdog owns expiry so rollback and repaint happen together.
const numeric = { parse: Number, serialize: String, ttlMs: Infinity };
const AMP_INTENT_TIMEOUT_MS = 3000;

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

interface AmpRequest {
	sequence: number;
	acceptedSequence: number;
	retiredThrough: number;
	timer: number | undefined;
}
let requestSequence = 0;
const requests: Partial<Record<AmpField, AmpRequest>> = {};
const fields = Object.keys(AMP_FIELDS) as AmpField[];

function renderAmpField(field: AmpField, force = false): void {
	const { id, slot } = AMP_FIELDS[field];
	const el = document.getElementById(id) as HTMLSelectElement | null;
	if (!el) return;
	if (force || document.activeElement !== el) el.value = String(slot.get());
	el.setAttribute('aria-busy', String(slot.pending()));
}

function finishAmpRequest(field: AmpField): void {
	const request = requests[field];
	if (request) {
		window.clearTimeout(request.timer);
		request.timer = undefined;
		request.retiredThrough = request.sequence;
	}
	AMP_FIELDS[field].slot.reset();
	renderAmpField(field, true);
	syncAmpAvailability(measurementOwnsDevice());
}

/** Only the newest request for a field may reject that field's intent. */
export function rejectAmpRequest(responseTo?: string, requestId?: string | number): void {
	if (responseTo !== 'SET_AMP') return;
	for (const field of fields) {
		const request = requests[field];
		if (request?.timer !== undefined && requestId === `amp-${field}-${request.sequence}`) {
			finishAmpRequest(field);
			return;
		}
	}
}

/** Disconnect: retire request IDs as well as clearing every pending slot/watchdog. */
export function resetAmpIntents(): void {
	for (const field of fields) finishAmpRequest(field);
}

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
	const previous = requests[field];
	window.clearTimeout(previous?.timer);
	const sequence = ++requestSequence;
	const requestId = `amp-${field}-${sequence}`;
	const request: AmpRequest = {
		sequence,
		acceptedSequence: previous?.acceptedSequence ?? 0,
		retiredThrough: previous?.retiredThrough ?? 0,
		timer: window.setTimeout(() => finishAmpRequest(field), AMP_INTENT_TIMEOUT_MS),
	};
	requests[field] = request;
	meta.slot.reset();
	meta.slot.set(v);
	renderAmpField(field, true);
	syncAmpAvailability(measurementOwnsDevice());
	if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
	send({ cmd: 'SET_AMP', [field]: v, request_id: requestId });
	return true;
}

/**
 * Requested fields confirm slots; actual fields only update readouts. A reply to an older
 * request can update its field's rollback baseline while a newer intent waits, but cannot
 * settle that intent or overwrite a newer accepted reply. Other fields in that reply's
 * snapshot cannot confirm independent requests.
 */
export function syncAmpStatus(amp: unknown, responseTo?: string, requestId?: string | number): void {
	if (!amp || typeof amp !== 'object') return;
	const a = amp as Record<string, unknown>;
	const ownReply = responseTo === 'SET_AMP' && typeof requestId === 'string'
		? /^amp-(atten|preamp|ifgain|gain_strategy)-(\d+)$/.exec(requestId) : null;
	const replyField = ownReply?.[1] as AmpField | undefined;
	const replySequence = ownReply ? Number(ownReply[2]) : 0;
	if (replyField) {
		const request = requests[replyField];
		if (!request || replySequence <= request.retiredThrough
			|| replySequence < request.acceptedSequence || replySequence > request.sequence) return;
	}
	for (const field of fields) {
		if (replyField && replyField !== field) continue;
		const meta = AMP_FIELDS[field];
		const v = ampNumber(a, field);
		if (v === null || !Number.isInteger(v) || v < meta.min || v > meta.max) continue;
		const request = requests[field];
		const desired = meta.slot.desiredValue();
		const matching = replyField === field && replySequence === request?.sequence;
		meta.slot.confirm(v);
		if (replyField && request) request.acceptedSequence = replySequence;
		if (matching) finishAmpRequest(field);
		else if (desired !== null) meta.slot.set(desired);
		renderAmpField(field);
	}

	const attenActual = ampNumber(a, 'atten_actual');
	const ca = document.getElementById('cur-atten');
	if (ca) {
		ca.textContent = attenActual !== null && attenActual >= 0 && attenActual <= 33 ? attenActual + ' dB' : '';
	}
	const preampActual = ampNumber(a, 'preamp_actual');
	const cp = document.getElementById('cur-preamp');
	if (cp) {
		cp.textContent = preampActual === 0 ? t('auto') : preampActual === 1 ? t('off')
			: preampActual !== null && Number.isInteger(preampActual) && preampActual >= 2 && preampActual <= 4
				? 'L' + preampActual : '';
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
