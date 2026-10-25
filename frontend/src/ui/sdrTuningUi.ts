/**
 * SDR tuning panel widgets: step-tier inputs and the Locate selector.
 *
 * Owns only the DOM. Values live in sdrState slots; this module renders them and reports
 * user edits through callbacks. It never sends commands: the settings change preferences,
 * and the parent decides what (if anything) goes to the device.
 */
import { t } from '../core/i18n';
import {
	currentSdrStepHz, effectiveLocateMode, sdrChannelAnchorHz, sdrChannelStepHz, sdrDemod,
	sdrLocateMode, type LocateMode, type TuneTier,
} from './sdrState';
import { signalLocateSupported } from './sdrTuning';

export interface SdrTuningCallbacks {
	/** A tier step was committed by the user. */
	onStepCommit(tier: TuneTier, hz: number): void;
	/** A locate mode was picked; `signal` arrives only when the current mode supports it. */
	onLocateSelect(mode: LocateMode): void;
	/** Grid spacing (Hz) and anchor (Hz) were committed. */
	onGridCommit(stepHz: number, anchorHz: number): void;
}

const LOCATE_MODES: readonly LocateMode[] = ['free', 'channel', 'signal'];
const input = (id: string) => document.getElementById(id) as HTMLInputElement | null;

let callbacks: SdrTuningCallbacks | null = null;
let lastRenderedMode = '';
let bound = false;

/** True while the user holds a draft in one of the settings inputs. */
const editing = (el: HTMLInputElement | null): boolean =>
	el !== null && document.activeElement === el && el.dataset.edited === '1';

// ── panels ──

function setStepPanel(open: boolean): void {
	if (!open) restoreInputs('steps');
	const panel = document.getElementById('sdr-step-settings-panel');
	if (panel) panel.style.display = open ? '' : 'none';
	document.getElementById('btn-sdr-step-settings')?.setAttribute('aria-expanded', String(open));
}

// ── inputs ──

/** Clear draft flags before blur can dispatch a commit. */
function restoreInputs(scope: 'steps' | 'grid' | 'all' = 'all'): void {
	const ids = scope === 'steps' ? ['input-sdr-step-coarse', 'input-sdr-step-fine']
		: scope === 'grid' ? ['input-sdr-locate-step', 'input-sdr-locate-anchor']
			: ['input-sdr-step-coarse', 'input-sdr-step-fine', 'input-sdr-locate-step', 'input-sdr-locate-anchor'];
	for (const id of ids) {
		const el = input(id);
		if (!el) continue;
		delete el.dataset.edited;
		el.classList.remove('invalid');
		if (id === 'input-sdr-step-coarse') el.value = stepText(currentSdrStepHz('coarse'));
		else if (id === 'input-sdr-step-fine') el.value = stepText(currentSdrStepHz('fine'));
		else if (id === 'input-sdr-locate-step') el.value = gridStepText(sdrChannelStepHz.get());
		else el.value = String(sdrChannelAnchorHz.get() / 1e6);
	}
	if (document.activeElement && ids.includes(document.activeElement.id)) {
		(document.activeElement as HTMLInputElement).blur();
	}
}

function stepText(hz: number): string {
	return String(Number(hz.toPrecision(6)));
}

function gridStepText(hz: number): string {
	return String(Number(hz.toPrecision(6)));
}

function commitStepInput(el: HTMLInputElement, tier: TuneTier): void {
	const hz = Number(el.value);
	if (!Number.isFinite(hz) || hz < 1) {
		el.classList.add('invalid');
		return;
	}
	el.classList.remove('invalid');
	const changed = Math.abs(hz - currentSdrStepHz(tier)) >= 0.5;
	delete el.dataset.edited;
	if (changed) callbacks?.onStepCommit(tier, hz);
	el.value = stepText(currentSdrStepHz(tier));
}

/**
 * Commit one grid field. The other value comes from its slot, not from the sibling input:
 * an uncommitted draft in the sibling box must never ride along with this commit.
 */
function commitGridInputs(which: 'step' | 'anchor'): void {
	const spacing = input('input-sdr-locate-step');
	const anchor = input('input-sdr-locate-anchor');
	let stepHz = sdrChannelStepHz.get();
	let anchorHz = sdrChannelAnchorHz.get();
	if (which === 'step' && spacing) {
		const parsed = Number(spacing.value);
		spacing.classList.toggle('invalid', !(Number.isFinite(parsed) && parsed >= 1));
		if (spacing.classList.contains('invalid')) return;
		stepHz = parsed;
		delete spacing.dataset.edited;
	} else if (which === 'anchor' && anchor) {
		const parsed = Number(anchor.value);
		anchor.classList.toggle('invalid', anchor.value.trim() === '' || !Number.isFinite(parsed * 1e6));
		if (anchor.classList.contains('invalid')) return;
		anchorHz = parsed * 1e6;
		delete anchor.dataset.edited;
	}
	if (stepHz !== sdrChannelStepHz.get() || anchorHz !== sdrChannelAnchorHz.get()) {
		callbacks?.onGridCommit(stepHz, anchorHz);
		return;
	}
	// No change: normalise the committed box back to its slot value.
	if (which === 'step' && spacing) spacing.value = gridStepText(stepHz);
	if (which === 'anchor' && anchor) anchor.value = String(anchorHz / 1e6);
}

function bindHzInput(el: HTMLInputElement, onCommit: () => void): void {
	el.addEventListener('input', () => {
		el.dataset.edited = '1';
		el.classList.remove('invalid');
	});
	const commitDraft = () => {
		if (el.dataset.edited === '1') onCommit();
	};
	el.addEventListener('change', commitDraft);
	el.addEventListener('blur', commitDraft);
	el.addEventListener('keydown', (event) => {
		if (event.key === 'Enter') {
			event.preventDefault();
			event.stopPropagation();
			onCommit();
		} else if (event.key === 'Escape') {
			event.preventDefault();
			event.stopPropagation();
			cancelSdrTuningEdit();
		}
	});
}

// ── locate buttons ──

function locateButtonLabel(mode: LocateMode): string {
	return t(`sdr_locate_${mode}`);
}

function buildLocateGroup(): void {
	const group = document.getElementById('sdr-locate-group');
	if (!group || group.childElementCount > 0) return;
	for (const mode of LOCATE_MODES) {
		const button = document.createElement('button');
		button.type = 'button';
		button.className = 'btn';
		button.dataset.sdrLocate = mode;
		button.textContent = locateButtonLabel(mode);
		if (mode === 'channel') button.setAttribute('aria-controls', 'sdr-locate-settings-panel');
		group.appendChild(button);
	}
	// Cancel before the pointer's focus change can commit a departing Channel draft.
	group.addEventListener('pointerdown', (event) => {
		const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('[data-sdr-locate]');
		if (button && !button.disabled && button.dataset.sdrLocate !== 'channel') restoreInputs('grid');
	});
	group.addEventListener('click', (event) => {
		const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('[data-sdr-locate]');
		if (!button || button.disabled) return;
		const mode = button.dataset.sdrLocate as LocateMode;
		if (mode === 'signal' && !signalLocateSupported(sdrDemod.get())) return;
		if (mode !== 'channel') restoreInputs('grid');
		if (mode !== sdrLocateMode.get()) callbacks?.onLocateSelect(mode);
	});
}

// ── public API ──

/**
 * Wire the widgets once. Called from the panel setup with the parent's commit callbacks;
 * safe to call again (existing bindings stay, only the callbacks are replaced).
 */
export function bindSdrTuningUi(next: SdrTuningCallbacks): void {
	callbacks = next;
	buildLocateGroup();
	if (bound) {
		renderSdrTuningUi();
		return;
	}
	bound = true;

	const stepToggle = document.getElementById('btn-sdr-step-settings');
	stepToggle?.addEventListener('pointerdown', () => {
		const panel = document.getElementById('sdr-step-settings-panel');
		if (panel && panel.style.display !== 'none') restoreInputs('steps');
	});
	stepToggle?.addEventListener('click', () => {
		const panel = document.getElementById('sdr-step-settings-panel');
		if (panel) setStepPanel(panel.style.display === 'none');
	});

	const coarse = input('input-sdr-step-coarse');
	if (coarse) bindHzInput(coarse, () => commitStepInput(coarse, 'coarse'));
	const fine = input('input-sdr-step-fine');
	if (fine) bindHzInput(fine, () => commitStepInput(fine, 'fine'));
	const spacing = input('input-sdr-locate-step');
	if (spacing) bindHzInput(spacing, () => commitGridInputs('step'));
	const anchor = input('input-sdr-locate-anchor');
	if (anchor) bindHzInput(anchor, () => commitGridInputs('anchor'));

	document.addEventListener('keydown', (event) => {
		if (event.key !== 'Escape') return;
		const stepPanel = document.getElementById('sdr-step-settings-panel');
		const stepsOpen = stepPanel && stepPanel.style.display !== 'none';
		const settingsFocused = document.activeElement?.closest('.sdr-inline-settings');
		if (stepsOpen || settingsFocused) {
			event.stopPropagation();
			event.preventDefault();
			cancelSdrTuningEdit();
		}
	}, true);

	renderSdrTuningUi();
}

/**
 * Write the widgets from the slots. A focused input keeps its draft; the mode readout is
 * refreshed even then, because the draft belongs to the new mode only after a commit.
 */
export function renderSdrTuningUi(): void {
	const mode = sdrDemod.get();
	if (lastRenderedMode !== mode) {
		lastRenderedMode = mode;
		restoreInputs();
	}
	const coarse = input('input-sdr-step-coarse');
	if (coarse && !editing(coarse)) coarse.value = stepText(currentSdrStepHz('coarse'));
	const fine = input('input-sdr-step-fine');
	if (fine && !editing(fine)) fine.value = stepText(currentSdrStepHz('fine'));
	const spacing = input('input-sdr-locate-step');
	if (spacing && !editing(spacing)) spacing.value = gridStepText(sdrChannelStepHz.get());
	const anchor = input('input-sdr-locate-anchor');
	if (anchor && !editing(anchor)) anchor.value = String(sdrChannelAnchorHz.get() / 1e6);

	// The active button shows the effective mode: an unsupported `signal` preference
	// renders as Free while the preference itself stays stored for the return.
	const effective = effectiveLocateMode(mode);
	const panel = document.getElementById('sdr-locate-settings-panel');
	if (panel) {
		if (effective !== 'channel' && panel.style.display !== 'none') restoreInputs('grid');
		panel.style.display = effective === 'channel' ? '' : 'none';
	}
	const group = document.getElementById('sdr-locate-group');
	if (group) {
		for (const button of Array.from(group.querySelectorAll<HTMLButtonElement>('[data-sdr-locate]'))) {
			const buttonMode = button.dataset.sdrLocate as LocateMode;
			const active = buttonMode === effective;
			button.classList.toggle('active', active);
			button.setAttribute('aria-pressed', String(active));
			button.textContent = locateButtonLabel(buttonMode);
			if (buttonMode === 'channel') button.setAttribute('aria-expanded', String(active));
			if (buttonMode !== 'signal') button.title = t(`sdr_locate_${buttonMode}_tip`);
			if (buttonMode === 'signal') {
				const supported = signalLocateSupported(mode);
				button.disabled = !supported;
				const hint = supported ? t('sdr_locate_hint') : t('sdr_locate_signal_unsupported');
				button.title = hint;
				button.setAttribute('aria-description', hint);
			}
		}
	}
}

/** Drop drafts and collapse Steps; Channel settings follow the selected locate mode. */
export function cancelSdrTuningEdit(): void {
	restoreInputs();
	setStepPanel(false);
}
