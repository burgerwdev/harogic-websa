import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SdrTuningCallbacks } from '../ui/sdrTuningUi';
import type { LocateMode, TuneTier } from '../ui/sdrState';
import type * as SdrTuningUi from '../ui/sdrTuningUi';
import type * as SdrState from '../ui/sdrState';

let ui: typeof SdrTuningUi;
let state: typeof SdrState;
let callbacks: SdrTuningCallbacks;
const listeners: Array<() => void> = [];

function element<T extends HTMLElement = HTMLElement>(id: string): T {
	return document.getElementById(id) as T;
}

function field(id: string): HTMLInputElement {
	return element<HTMLInputElement>(id);
}

function locate(mode: LocateMode): HTMLButtonElement {
	return document.querySelector<HTMLButtonElement>(`[data-sdr-locate="${mode}"]`)!;
}

function draft(id: string, value: string): HTMLInputElement {
	const input = field(id);
	input.focus();
	input.value = value;
	input.dispatchEvent(new Event('input', { bubbles: true }));
	return input;
}

function key(input: HTMLElement, value: string): void {
	input.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true }));
}

function pointerdown(target: HTMLElement): void {
	target.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true }));
}

beforeEach(async () => {
	vi.resetModules();
	localStorage.clear();
	// Keep the real panel nesting and input types. Channel has no separate Grid trigger.
	document.body.innerHTML = `
		<button id="btn-sdr-step-settings" type="button" aria-expanded="false"
			aria-controls="sdr-step-settings-panel">Steps</button>
		<div id="sdr-step-settings-panel" class="sdr-inline-settings" style="display:none">
			<input id="input-sdr-step-coarse" type="number" min="1" step="any">
			<input id="input-sdr-step-fine" type="number" min="1" step="any">
		</div>
		<div id="sdr-locate-group" class="btn-group"></div>
		<div id="sdr-locate-settings-panel" class="sdr-inline-settings" style="display:none">
			<input id="input-sdr-locate-step" type="number" min="1" step="any">
			<input id="input-sdr-locate-anchor" type="number" step="any">
		</div>
		<button id="outside" type="button">Outside</button>`;
	const add = EventTarget.prototype.addEventListener;
	vi.spyOn(EventTarget.prototype, 'addEventListener').mockImplementation(function (this: EventTarget, type, listener, options) {
		add.call(this, type, listener, options);
		if (this === document || this === window) {
			const target = this;
			listeners.push(() => target.removeEventListener(type, listener, options));
		}
	});
	// Reload the binding boundary: the module intentionally binds once for each document.
	// Static imports would retain its bound flag after replacing the fixture between tests.
	state = await import('../ui/sdrState');
	ui = await import('../ui/sdrTuningUi');
	state.sdrDemod.set('am');
	state.sdrLocateMode.set('free');
	state.sdrChannelStepHz.set(12500);
	state.sdrChannelAnchorHz.set(145.5e6);
	state.setSdrStepForCurrentMode(10000, 'coarse');
	state.setSdrStepForCurrentMode(25, 'fine');
	callbacks = {
		onStepCommit: vi.fn((tier: TuneTier, hz: number) => {
			state.setSdrStepForCurrentMode(hz, tier);
			ui.renderSdrTuningUi();
		}),
		onLocateSelect: vi.fn((mode: LocateMode) => {
			state.sdrLocateMode.set(mode);
			ui.renderSdrTuningUi();
		}),
		onGridCommit: vi.fn((stepHz: number, anchorHz: number) => {
			state.sdrChannelStepHz.set(stepHz);
			state.sdrChannelAnchorHz.set(anchorHz);
			ui.renderSdrTuningUi();
		}),
	};
	ui.bindSdrTuningUi(callbacks);
});

afterEach(() => {
	listeners.splice(0).forEach((remove) => remove());
	vi.restoreAllMocks();
	document.body.innerHTML = '';
	localStorage.clear();
});

describe('Channel settings visibility and draft ownership', () => {
	it('reveals Channel settings directly and retains them after render and outside interaction', () => {
		expect(element('sdr-locate-settings-panel').style.display).toBe('none');
		locate('channel').click();
		expect(callbacks.onLocateSelect).toHaveBeenCalledExactlyOnceWith('channel');
		expect(element('sdr-locate-settings-panel').style.display).not.toBe('none');
		expect(locate('channel').getAttribute('aria-expanded')).toBe('true');
		expect(field('input-sdr-locate-step').valueAsNumber).toBe(12500);
		expect(field('input-sdr-locate-anchor').valueAsNumber).toBe(145.5);
		ui.renderSdrTuningUi();
		pointerdown(element('outside'));
		element('outside').focus();
		element('outside').click();
		ui.renderSdrTuningUi();
		expect(element('sdr-locate-settings-panel').style.display).not.toBe('none');
		expect(state.sdrLocateMode.get()).toBe('channel');
	});

	it.each((['free', 'signal'] as const).flatMap((mode) => [
		{ mode, id: 'input-sdr-locate-step', value: '6250', committed: 12500 },
		{ mode, id: 'input-sdr-locate-anchor', value: '144.125', committed: 145.5 },
	]))('cancels $id on $mode pointerdown before blur or change can commit', ({ mode, id, value, committed }) => {
		locate('channel').click();
		const spacing = draft(id, value);
		const observedAtBlur: number[] = [];
		spacing.addEventListener('blur', () => observedAtBlur.push(spacing.valueAsNumber));
		pointerdown(locate(mode));
		expect(observedAtBlur).toEqual([committed]);
		expect(spacing.valueAsNumber).toBe(committed);
		expect(callbacks.onGridCommit).not.toHaveBeenCalled();
		spacing.dispatchEvent(new Event('change', { bubbles: true }));
		spacing.blur();
		locate(mode).click();
		expect(callbacks.onGridCommit).not.toHaveBeenCalled();
		expect(state.sdrLocateMode.get()).toBe(mode);
		expect(element('sdr-locate-settings-panel').style.display).toBe('none');
		locate('channel').click();
		expect(element('sdr-locate-settings-panel').style.display).not.toBe('none');
		expect(spacing.valueAsNumber).toBe(committed);
		expect(state.sdrChannelStepHz.get()).toBe(12500);
		expect(state.sdrChannelAnchorHz.get()).toBe(145.5e6);
	});

	it('restores the new demod mode step before blurring the previous mode draft', () => {
		element('btn-sdr-step-settings').click();
		const coarse = draft('input-sdr-step-coarse', '4321');
		const observedAtBlur: number[] = [];
		coarse.addEventListener('blur', () => observedAtBlur.push(coarse.valueAsNumber));
		state.sdrDemod.set('cw');
		const cwStep = state.currentSdrStepHz('coarse');
		ui.renderSdrTuningUi();
		expect(observedAtBlur).toEqual([cwStep]);
		coarse.dispatchEvent(new Event('change', { bubbles: true }));
		coarse.blur();
		expect(callbacks.onStepCommit).not.toHaveBeenCalled();
		expect(state.currentSdrStepHz('coarse')).toBe(cwStep);
		state.sdrDemod.set('am');
		ui.renderSdrTuningUi();
		expect(coarse.valueAsNumber).toBe(10000);
	});

	it('toggles Steps independently and cancels its draft before closing without hiding Channel settings', () => {
		locate('channel').click();
		const toggle = element('btn-sdr-step-settings');
		toggle.click();
		expect(element('sdr-step-settings-panel').style.display).not.toBe('none');
		const coarse = draft('input-sdr-step-coarse', '4321');
		pointerdown(toggle);
		expect(coarse.valueAsNumber).toBe(10000);
		coarse.dispatchEvent(new Event('change', { bubbles: true }));
		toggle.click();
		expect(callbacks.onStepCommit).not.toHaveBeenCalled();
		expect(element('sdr-step-settings-panel').style.display).toBe('none');
		expect(element('sdr-locate-settings-panel').style.display).not.toBe('none');
		toggle.click();
		locate('free').click();
		expect(element('sdr-step-settings-panel').style.display).not.toBe('none');
		expect(element('sdr-locate-settings-panel').style.display).toBe('none');
	});

	it.each(['input-sdr-step-coarse', 'input-sdr-locate-step', 'input-sdr-locate-anchor'])(
		'Escape cancels %s while keeping the selected Channel settings visible', (id) => {
			locate('channel').click();
			element('btn-sdr-step-settings').click();
			const committed = field(id).valueAsNumber;
			const input = draft(id, '4321');
			key(input, 'Escape');
			input.dispatchEvent(new Event('change', { bubbles: true }));
			input.blur();
			expect(input.valueAsNumber).toBe(committed);
			expect(callbacks.onStepCommit).not.toHaveBeenCalled();
			expect(callbacks.onGridCommit).not.toHaveBeenCalled();
			expect(state.sdrLocateMode.get()).toBe('channel');
			expect(element('sdr-locate-settings-panel').style.display).not.toBe('none');
			expect(element('sdr-step-settings-panel').style.display).toBe('none');
		});
});

describe('settings commits', () => {
	it.each([
		{ id: 'input-sdr-step-coarse', value: '7500', tier: 'coarse' },
		{ id: 'input-sdr-step-fine', value: '12.5', tier: 'fine' },
		{ id: 'input-sdr-locate-step', value: '6250', tier: null },
		{ id: 'input-sdr-locate-anchor', value: '144.125', tier: null },
	] as const)('commits $id exactly once through Enter, change and blur', ({ id, value, tier }) => {
		locate('channel').click();
		element('btn-sdr-step-settings').click();
		const input = draft(id, value);
		key(input, 'Enter');
		input.dispatchEvent(new Event('change', { bubbles: true }));
		input.blur();
		if (tier) {
			expect(callbacks.onStepCommit).toHaveBeenCalledExactlyOnceWith(tier, Number(value));
			expect(callbacks.onGridCommit).not.toHaveBeenCalled();
			expect(state.currentSdrStepHz(tier)).toBe(Number(value));
		} else {
			const expectedStep = id === 'input-sdr-locate-step' ? 6250 : 12500;
			const expectedAnchor = id === 'input-sdr-locate-anchor' ? 144.125e6 : 145.5e6;
			expect(callbacks.onGridCommit).toHaveBeenCalledExactlyOnceWith(expectedStep, expectedAnchor);
			expect(callbacks.onStepCommit).not.toHaveBeenCalled();
			expect(state.sdrChannelStepHz.get()).toBe(expectedStep);
			expect(state.sdrChannelAnchorHz.get()).toBe(expectedAnchor);
		}
	});

	it.each([
		['input-sdr-step-coarse', '0'],
		['input-sdr-step-fine', '-1'],
		['input-sdr-locate-step', ''],
		['input-sdr-locate-step', '0'],
		['input-sdr-locate-anchor', ''],
		['input-sdr-locate-anchor', 'not-a-number'],
		['input-sdr-locate-anchor', '1e308'],
	])('does not commit invalid %s value %j', (id, value) => {
		locate('channel').click();
		element('btn-sdr-step-settings').click();
		const input = draft(id, value);
		key(input, 'Enter');
		input.dispatchEvent(new Event('change', { bubbles: true }));
		input.blur();
		expect(callbacks.onStepCommit).not.toHaveBeenCalled();
		expect(callbacks.onGridCommit).not.toHaveBeenCalled();
		expect(state.sdrChannelStepHz.get()).toBe(12500);
		expect(state.sdrChannelAnchorHz.get()).toBe(145.5e6);
	});

	it('replaces callbacks on repeat binding without duplicate selections, commits or toggles', () => {
		const replacement: SdrTuningCallbacks = {
			onStepCommit: vi.fn(), onLocateSelect: vi.fn(), onGridCommit: vi.fn(),
		};
		ui.bindSdrTuningUi(replacement);
		ui.bindSdrTuningUi(replacement);
		locate('channel').click();
		expect(replacement.onLocateSelect).toHaveBeenCalledExactlyOnceWith('channel');
		expect(callbacks.onLocateSelect).not.toHaveBeenCalled();
		element('btn-sdr-step-settings').click();
		expect(element('sdr-step-settings-panel').style.display).not.toBe('none');
		const coarse = draft('input-sdr-step-coarse', '7500');
		key(coarse, 'Enter');
		coarse.blur();
		expect(replacement.onStepCommit).toHaveBeenCalledExactlyOnceWith('coarse', 7500);
		expect(callbacks.onStepCommit).not.toHaveBeenCalled();
		state.sdrLocateMode.set('channel');
		ui.renderSdrTuningUi();
		const spacing = draft('input-sdr-locate-step', '6250');
		key(spacing, 'Enter');
		spacing.blur();
		expect(replacement.onGridCommit).toHaveBeenCalledExactlyOnceWith(6250, 145.5e6);
		expect(callbacks.onGridCommit).not.toHaveBeenCalled();
	});
});
