/**
 * Mode-aware SDR Filter control.
 *
 * The existing sdrIfbw slot remains the only receiver parameter. This module owns only the
 * panel's draft/preview and the per-mode confirmed preference map in sdrState; controls.ts
 * owns command submission, acknowledgement and rollback.
 */
import { t } from '../core/i18n';
import { demodBandHz, sdrDemod, sdrFilterByMode, sdrFilterLimits, sdrIfbw, sdrListenHz } from './sdrState';
import { getCapture, passbandViewActive } from './spectrumViewport';
import { convertUnitValue, toUnit, units } from '../core/units';
import type { UnitCommitDetail } from '../core/units';
import { modeDescriptor } from '../sdr/registry';

export interface SdrFilterSpec {
	editable: boolean;
	defaultHz: number;
	presets: readonly number[];
	unit: 'Hz' | 'kHz';
	/** i18n key describing what the numeric value means for this mode. */
	labelKey: string;
	/** The native DSP's real lower bound. SSB needs a positive guard, so it is 101 Hz. */
	minimumHz: number;
	maximumHz?: number;
}

// Labels describe native display semantics; all bandwidth geometry comes from the catalog.
const FILTER_LABELS: Record<string, string> = {
	usb: 'sdr_filter_high_cutoff', lsb: 'sdr_filter_high_cutoff',
	ft8: 'ft8_filter_fixed', sstv: 'sstv_filter_fixed', radiofax: 'radiofax_filter_fixed',
	drm: 'drm_filter_fixed', drmplus: 'drmplus_filter_fixed',
};
const UNKNOWN_SPEC: SdrFilterSpec = {
	editable: false, defaultHz: 6_000, presets: [], unit: 'kHz',
	labelKey: 'sdr_filter_bandwidth', minimumHz: 1,
};

export function filterSpec(mode: string): SdrFilterSpec {
	const descriptor = modeDescriptor(mode);
	if (descriptor) return {
		editable: descriptor.bandwidthEditable,
		defaultHz: descriptor.defaultBandwidthHz,
		presets: descriptor.bandwidthPresetsHz,
		unit: descriptor.defaultBandwidthHz < 1_000 ? 'Hz' : 'kHz',
		labelKey: FILTER_LABELS[mode] ?? 'sdr_filter_bandwidth',
		minimumHz: descriptor.minBandwidthHz,
		maximumHz: descriptor.maxBandwidthHz,
	};
	return UNKNOWN_SPEC;
}

/** Shared admission bounds for numeric and spectrum-edge controls. */
export function filterRange(mode: string): { min: number; max: number | null } {
	const specForMode = filterSpec(mode);
	const limits = sdrFilterLimits.get();
	return {
		min: Math.max(specForMode.minimumHz, limits?.mode === mode ? limits.min_hz : 0),
		max: limits?.mode === mode && limits.max_hz === null ? null
			: Math.min(specForMode.maximumHz ?? Infinity, limits?.mode === mode ? limits.max_hz ?? Infinity : Infinity),
	};
}

/**
 * Confirmed per-mode preference when finite and above the mode's real DSP minimum. Backend
 * rate-plan/admission remains the authority for all wider dynamic limits; controls retries a
 * rejected restored value with this mode's recommendation once.
 */
export function filterValueForMode(mode: string): number {
	const remembered = sdrFilterByMode.get()[mode];
	return remembered !== undefined && filterSpec(mode).editable
		&& Number.isFinite(remembered) && remembered >= filterSpec(mode).minimumHz
		&& remembered <= (filterSpec(mode).maximumHz ?? Infinity)
		? remembered : filterSpec(mode).defaultHz;
}

/** STATUS is the only caller: pending text or previews never become mode preferences. */
export function rememberConfirmedFilter(mode: string, hz: number): void {
	if (!filterSpec(mode).editable || !Number.isFinite(hz) || hz < filterSpec(mode).minimumHz
		|| hz > (filterSpec(mode).maximumHz ?? Infinity)) return;
	if (sdrFilterByMode.get()[mode] !== hz) sdrFilterByMode.set({ ...sdrFilterByMode.get(), [mode]: hz });
}

let previewHz: number | null = null;

export function filterPreviewHz(): number {
	return previewHz ?? sdrIfbw.get();
}

/** Pointer moves pass a finite Hz value; ending/cancelling passes null. */
export function setFilterPreview(hz: number | null): void {
	previewHz = hz !== null && Number.isFinite(hz) && hz > 0 ? hz : null;
	renderSdrFilter();
}

interface InputError {
	key: string;
	params?: Record<string, string | number>;
}

let callbacks: { onCommit: (hz: number) => void; onView: () => void } | null = null;
let inputError: InputError | null = null;
let suppressBlurCommit = false;
let renderedMode: string | null = null;
const bound = new WeakSet<HTMLElement>();
let unitCommitBound = false;

function input(): HTMLInputElement | null {
	return document.getElementById('input-sdr-ifbw') as HTMLInputElement | null;
}

function compactNumber(value: number): string {
	return String(Number(value.toPrecision(9)));
}

function magnitudeText(hz: number, unit: 'Hz' | 'kHz'): string {
	return compactNumber(unit === 'Hz' ? hz : hz / 1_000);
}

function inputText(hz: number): string {
	return compactNumber(toUnit(hz, 'sdr_ifbw'));
}

function inputValueText(hz: number): string {
	return inputText(hz) + ' ' + (units().sdr_ifbw ?? 'kHz');
}

function valueText(hz: number, specForMode: SdrFilterSpec): string {
	return magnitudeText(hz, specForMode.unit) + ' ' + specForMode.unit;
}

function edgeText(hz: number, specForMode: SdrFilterSpec): string {
	const sign = hz < 0 ? '−' : hz > 0 ? '+' : '';
	return sign + magnitudeText(Math.abs(hz), specForMode.unit);
}

function filterTooltip(mode: string, specForMode: SdrFilterSpec): string {
	if (mode === 'drm') return t('drm_filter_tip');
	if (mode === 'drmplus') return t('drmplus_filter_tip');
	return t(specForMode.labelKey);
}

function clearInputError(el: HTMLInputElement): void {
	inputError = null;
	el.classList.remove('invalid');
	el.setAttribute('aria-invalid', 'false');
	el.setCustomValidity('');
}

function rejectInput(el: HTMLInputElement, key: string,
	params?: Record<string, string | number>): void {
	inputError = { key, params };
	const message = t(key, params);
	el.classList.add('invalid');
	el.setAttribute('aria-invalid', 'true');
	el.setCustomValidity(message);
	// The hover text with this error is assembled by the render below.
	renderSdrFilter();
}

function parseInputHz(el: HTMLInputElement): number {
	const raw = el.value.trim();
	const parsed = raw === '' ? NaN : Number(raw);
	return convertUnitValue(parsed, units().sdr_ifbw ?? 'kHz', 'Hz');
}

function commitInput(el: HTMLInputElement): void {
	if (suppressBlurCommit || previewHz !== null) return;
	const mode = sdrDemod.get();
	const specForMode = filterSpec(mode);
	if (!specForMode.editable || !callbacks) return;
	const hz = parseInputHz(el);
	if (!Number.isFinite(hz) || hz <= 0) {
		rejectInput(el, 'sdr_filter_invalid');
		return;
	}
	if (hz + 1e-9 < specForMode.minimumHz) {
		rejectInput(el, 'sdr_filter_too_narrow', { min: valueText(specForMode.minimumHz, specForMode) });
		return;
	}
	const limits = filterRange(mode);
	if (limits.max === null || hz < limits.min || hz > limits.max) {
		rejectInput(el, limits.max === null ? 'sdr_filter_no_range' : 'sdr_filter_outside_range', {
			min: inputValueText(limits.min), max: inputValueText(limits.max ?? 0),
		});
		return;
	}
	clearInputError(el);
	delete el.dataset.edited;
	// change and blur both use this path (the keypad commits through change only); an unchanged
	// value is not a second command.
	if (Math.abs(hz - sdrIfbw.get()) < 0.5) return;
	callbacks.onCommit(hz);
}

/** Drop an input draft without committing it (Preset, disconnect and every mode change). */
export function cancelSdrFilterEdit(): void {
	previewHz = null;
	const el = input();
	if (!el) {
		inputError = null;
		return;
	}
	suppressBlurCommit = true;
	try {
		el.blur();
	} finally {
		suppressBlurCommit = false;
	}
	el.value = inputText(filterPreviewHz());
	clearInputError(el);
	delete el.dataset.edited;
	renderSdrFilter();
}

function syncPresetButtons(group: HTMLElement, mode: string, specForMode: SdrFilterSpec,
	shownHz: number): void {
	if (group.dataset.mode !== mode || group.children.length !== specForMode.presets.length) {
		const buttons: HTMLButtonElement[] = [];
		for (const preset of specForMode.presets) {
			const button = document.createElement('button');
			button.type = 'button';
			button.className = 'btn';
			button.dataset.sdrIfbw = String(preset);
			button.textContent = preset >= 1_000_000 ? compactNumber(preset / 1_000_000) + 'M'
				: preset >= 1_000 ? compactNumber(preset / 1_000) + 'k' : compactNumber(preset);
			button.title = preset.toLocaleString('en-US') + ' Hz';
			buttons.push(button);
		}
		group.replaceChildren(...buttons);
		group.dataset.mode = mode;
	}
	for (const [index, child] of Array.from(group.children).entries()) {
		const button = child as HTMLButtonElement;
		const preset = specForMode.presets[index];
		const active = Math.abs(shownHz - preset) < 0.5;
		const range = filterRange(mode);
		button.disabled = !specForMode.editable || range.max === null || preset < range.min || preset > range.max;
		button.classList.toggle('active', active);
		button.setAttribute('aria-pressed', String(active));
	}
}

export function renderSdrFilter(): void {
	const mode = sdrDemod.get();
	const specForMode = filterSpec(mode);
	const shownHz = filterPreviewHz();
	const range = filterRange(mode);
	const el = input();
	if (el && document.activeElement === el && renderedMode !== null && renderedMode !== mode) {
		cancelSdrFilterEdit();
	}
	renderedMode = mode;
	const label = document.querySelector('label[for="input-sdr-ifbw"]');
	if (label) label.textContent = t(mode === 'usb' || mode === 'lsb' ? 'sdr_filter_cutoff_label' : 'if_filter');

	const group = document.getElementById('sdr-ifbw-group');
	if (group) syncPresetButtons(group, mode, specForMode, shownHz);

	// The input hover and description carry the mode meaning, actual passband, state and range.
	const [bandLo, bandHi] = demodBandHz(mode, shownHz);
	let stateText = '';
	if (inputError) {
		stateText = t(inputError.key, inputError.params);
	} else if (previewHz !== null) {
		stateText = t('sdr_filter_preview', { value: valueText(previewHz, specForMode) });
	} else if (sdrIfbw.pending()) {
		const confirmed = sdrIfbw.confirmedValue();
		stateText = confirmed === null
			? t('sdr_filter_pending', { value: valueText(shownHz, specForMode) })
			: t('sdr_filter_pending_confirmed', {
				value: valueText(shownHz, specForMode),
				confirmed: valueText(confirmed, specForMode),
			});
	} else if (specForMode.editable) {
		stateText = t('sdr_filter_confirmed', { value: valueText(shownHz, specForMode) });
	}
	const rangeText = specForMode.editable && (range.max === null || Number.isFinite(range.max))
		? range.max === null ? t('sdr_filter_no_range')
			: t('sdr_filter_range', { min: inputValueText(range.min), max: inputValueText(range.max) })
		: '';
	const passbandText = t('passband') + ': ' + t('sdr_filter_passband_value', {
		lo: edgeText(bandLo, specForMode), hi: edgeText(bandHi, specForMode), unit: specForMode.unit,
	});
	const hoverText = [filterTooltip(mode, specForMode), passbandText, stateText, rangeText].filter(Boolean).join('\n');
	const state = inputError ? 'error'
		: previewHz !== null ? 'preview'
		: sdrIfbw.pending() ? 'pending'
		: !specForMode.editable ? 'locked' : 'confirmed';
	if (el) {
		el.disabled = !specForMode.editable || range.max === null;
		el.min = String(toUnit(range.min, 'sdr_ifbw'));
		el.max = range.max !== null && Number.isFinite(range.max) ? String(toUnit(range.max, 'sdr_ifbw')) : '';
		el.step = 'any';
		el.dataset.unit = units().sdr_ifbw ?? 'kHz';
		// Only an edited draft survives STATUS/rejection renders. A successful commit clears the
		// edited mark, so a later failure can restore the confirmed value even while focused.
		const editing = document.activeElement === el && el.dataset.edited === '1';
		if (!editing && !inputError) {
			el.value = inputText(shownHz);
			clearInputError(el);
		}
		el.classList.toggle('pending', previewHz === null && sdrIfbw.pending());
		el.classList.toggle('preview', previewHz !== null);
		el.classList.toggle('locked', !specForMode.editable);
		el.dataset.state = state;
		el.title = hoverText;
		el.setAttribute('aria-description', hoverText);
	}

	const view = document.getElementById('btn-sdr-filter-view') as HTMLButtonElement | null;
	if (view) {
		const active = passbandViewActive();
		view.textContent = t('sdr_filter_view');
		const capture = getCapture();
		const listen = sdrListenHz.get();
		const available = capture !== null && listen + bandHi > capture.lo && listen + bandLo < capture.hi;
		view.title = t(active ? 'sdr_filter_restore_tip' : available ? 'sdr_filter_view_tip' : 'sdr_filter_view_unavailable');
		view.setAttribute('aria-label', t(active ? 'sdr_filter_restore' : 'sdr_filter_view_label'));
		view.setAttribute('aria-pressed', String(active));
		view.classList.toggle('active', active);
		view.disabled = !active && (!available || !Number.isFinite(shownHz) || shownHz <= 0);
	}
}

/** Event delegation survives the preset buttons being rebuilt on every mode change. */
export function bindSdrFilter(next: { onCommit: (hz: number) => void; onView: () => void }): void {
	callbacks = next;
	if (!unitCommitBound) {
		unitCommitBound = true;
		const owner = input();
		document.addEventListener('websa:unit-commit', (event) => {
			const detail = (event as CustomEvent<UnitCommitDetail>).detail;
			if (detail.field !== 'sdr_ifbw' || input() !== owner) return;
			const el = input();
			if (el && detail.commit) commitInput(el);
			renderSdrFilter();
		});
	}
	const unitGroup = document.getElementById('unit-sdr_ifbw-group');
	if (unitGroup && !bound.has(unitGroup)) {
		bound.add(unitGroup);
		// Keep an edited value in the field until the selected unit interprets it.
		unitGroup.addEventListener('mousedown', (event) => { event.preventDefault(); });
	}
	const group = document.getElementById('sdr-ifbw-group');
	if (group && !bound.has(group)) {
		bound.add(group);
		group.addEventListener('click', (event) => {
			const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('[data-sdr-ifbw]');
			if (!button || button.disabled) return;
			const hz = Number(button.dataset.sdrIfbw);
			if (!Number.isFinite(hz)) return;
			setFilterPreview(null);
			callbacks?.onCommit(hz);
		});
	}
	const el = input();
	if (el && !bound.has(el)) {
		bound.add(el);
		el.addEventListener('input', () => {
			el.dataset.edited = '1';
			if (inputError) clearInputError(el);
		});
		// Enter and native blur both commit. The virtual keypad only dispatches change; the
		// unchanged-value guard makes any following blur a no-op rather than a duplicate command.
		el.addEventListener('change', () => commitInput(el));
		el.addEventListener('blur', () => {
			if (!suppressBlurCommit && el.dataset.edited === '1') commitInput(el);
		});
		el.addEventListener('keydown', (event) => {
			if (event.key === 'Enter') {
				event.preventDefault();
				event.stopPropagation();
				commitInput(el);
			} else if (event.key === 'Escape') {
				event.preventDefault();
				event.stopPropagation();
				cancelSdrFilterEdit();
			}
		});
	}
	const view = document.getElementById('btn-sdr-filter-view');
	if (view && !bound.has(view)) {
		bound.add(view);
		view.addEventListener('click', () => callbacks?.onView());
	}
	renderSdrFilter();
}
