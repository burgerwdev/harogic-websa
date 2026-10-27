import { t } from '../core/i18n';
import { sdrDemod, sdrPitchHz } from './sdrState';
import { requestRender } from '../render/redraw';

let onCommit: ((hz: number) => void) | null = null;
let preview: number | null = null;
let suppressCommit = false;
const bound = new WeakSet<HTMLElement>();
const field = () => document.getElementById('input-sdr-pitch') as HTMLInputElement | null;
const slider = () => document.getElementById('slider-sdr-pitch') as HTMLInputElement | null;

/** Local slider preview; receiver state changes only after release. */
export function tonePreviewHz(): number {
	return preview ?? sdrPitchHz.get();
}

export function cancelSdrToneEdit(): void {
	preview = null;
	const input = field();
	if (input) {
		delete input.dataset.edited;
		input.setCustomValidity('');
		input.removeAttribute('aria-invalid');
		suppressCommit = true;
		input.blur();
		suppressCommit = false;
	}
	renderSdrTone();
	requestRender();
}

export function renderSdrTone(): void {
	const available = sdrDemod.get() === 'cw';
	const panel = document.getElementById('sdr-tone-controls');
	if (panel) panel.hidden = !available;
	if (!available) preview = null;
	const hz = preview ?? sdrPitchHz.get();
	const input = field();
	if (input) {
		input.disabled = !available;
		if (!available || input.dataset.edited !== '1') input.value = String(hz);
		input.classList.toggle('pending', sdrPitchHz.pending());
		input.setAttribute('aria-busy', String(sdrPitchHz.pending()));
		input.title = t('sdr_tone_tip');
	}
	const range = slider();
	if (range) {
		range.disabled = !available;
		range.value = String(hz);
		range.setAttribute('aria-valuetext', `${hz} Hz`);
		range.setAttribute('aria-label', t('sdr_tone_label'));
	}
	const status = document.getElementById('sdr-tone-status');
	if (status) status.textContent = input?.getAttribute('aria-invalid') === 'true'
		? t('sdr_tone_range') : t(preview !== null ? 'sdr_filter_preview'
			: sdrPitchHz.pending() ? 'sdr_filter_pending' : 'sdr_filter_confirmed', { value: `${hz} Hz` });
}

function commit(hz: number): void {
	if (suppressCommit || sdrDemod.get() !== 'cw') return;
	const input = field();
	if (!Number.isFinite(hz) || hz < 100 || hz > 1600) {
		input?.setCustomValidity(t('sdr_tone_range'));
		input?.setAttribute('aria-invalid', 'true');
		renderSdrTone();
		return;
	}
	preview = null;
	if (input) {
		delete input.dataset.edited;
		input.setCustomValidity('');
		input.removeAttribute('aria-invalid');
	}
	if (hz !== sdrPitchHz.get()) onCommit?.(hz);
	renderSdrTone();
}

export function bindSdrTone(commitPitch: (hz: number) => void): void {
	onCommit = commitPitch;
	const input = field();
	if (input && !bound.has(input)) {
		bound.add(input);
		input.addEventListener('input', () => {
			input.dataset.edited = '1';
			input.setCustomValidity('');
			input.removeAttribute('aria-invalid');
		});
		input.addEventListener('change', () => commit(input.valueAsNumber));
		input.addEventListener('blur', () => {
			if (input.dataset.edited === '1') commit(input.valueAsNumber);
		});
		input.addEventListener('keydown', (event) => {
			if (event.key !== 'Escape' && event.key !== 'Enter') return;
			event.preventDefault();
			event.stopPropagation();
			if (event.key === 'Escape') cancelSdrToneEdit();
			else commit(input.valueAsNumber);
		});
	}
	const range = slider();
	if (range && !bound.has(range)) {
		bound.add(range);
		range.addEventListener('pointerdown', () => {
			const input = field();
			if (input) {
				delete input.dataset.edited;
				input.setCustomValidity('');
				input.removeAttribute('aria-invalid');
			}
		});
		range.addEventListener('input', () => { preview = range.valueAsNumber; renderSdrTone(); requestRender(); });
		range.addEventListener('change', () => commit(range.valueAsNumber));
		range.addEventListener('pointercancel', cancelSdrToneEdit);
		range.addEventListener('keydown', (event) => {
			if (event.key === 'Escape') {
				event.preventDefault();
				event.stopPropagation();
				cancelSdrToneEdit();
			}
		});
	}
	renderSdrTone();
}
