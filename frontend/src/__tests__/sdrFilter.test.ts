import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const markup = `
	<input id="input-sdr-ifbw"><div id="unit-sdr_ifbw-group"></div>
	<div id="sdr-ifbw-group"></div><button id="btn-sdr-filter-view"></button>
	<input type="range" id="slider-sdr-ifbw"><output id="sdr-filter-readout"></output>
	<span id="sdr-filter-status"></span>`;

beforeEach(() => {
	vi.resetModules();
	localStorage.clear();
	document.body.innerHTML = markup;
});

afterEach(() => {
	document.body.innerHTML = '';
	localStorage.clear();
});
// Reload modules per case so constructor-time localStorage parsing and in-memory mode maps are isolated.

async function load(mode = 'am', ifbw = 6000) {
	const state = await import('../ui/sdrState');
	state.sdrDemod.confirm(mode);
	state.sdrIfbw.confirm(ifbw);
	state.sdrListenHz.confirm(100_000_000);
	// Match the viewport instance loaded after resetModules in this case.
	const viewport = await import('../ui/spectrumViewport');
	viewport.setCapture({ lo: 99_000_000, hi: 101_000_000 });
	const filter = await import('../ui/sdrFilter');
	return { state, filter };
}

describe('SDR Filter panel', () => {
	it('previews slider movement locally and commits once on release within reported limits', async () => {
		const { state, filter } = await load();
		state.sdrFilterLimits.confirm({ mode: 'am', min_hz: 100, max_hz: 9000 });
		const commit = vi.fn((hz: number) => state.sdrIfbw.set(hz));
		filter.bindSdrFilter({ onCommit: commit, onView: vi.fn() });
		const slider = document.getElementById('slider-sdr-ifbw') as HTMLInputElement;
		expect(slider.min).toBe('100');
		expect(slider.max).toBe('9000');
		slider.value = '8000';
		slider.dispatchEvent(new Event('input', { bubbles: true }));
		expect(filter.filterPreviewHz()).toBe(8000);
		expect(state.sdrIfbw.get()).toBe(6000);
		expect(commit).not.toHaveBeenCalled();
		slider.dispatchEvent(new Event('change', { bubbles: true }));
		expect(commit).toHaveBeenCalledExactlyOnceWith(8000);
	});
	it('renders the actual AM passband and applies a preset through the shared IF slot', async () => {
		const { state, filter } = await load();
		const onView = vi.fn();
		filter.bindSdrFilter({
			onCommit: (hz) => { state.sdrIfbw.set(hz); filter.renderSdrFilter(); },
			onView,
		});
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		expect(input.value).toBe('6');
		expect(input.dataset.unit).toBe('kHz');
		expect(input.dataset.state).toBe('confirmed');
		expect(input.title).toContain('−3');
		expect(input.title).toContain('+3');
		const preset = document.querySelector<HTMLButtonElement>('[data-sdr-ifbw="4500"]')!
		preset.click();
		expect(state.sdrIfbw.get()).toBe(4500);
		expect(state.sdrIfbw.desiredValue()).toBe(4500);
		expect(document.querySelector<HTMLButtonElement>('[data-sdr-ifbw="4500"]')!
			.getAttribute('aria-pressed')).toBe('true');
		document.getElementById('btn-sdr-filter-view')!.click();
		expect(onView).toHaveBeenCalledOnce();
	});

	it('locks fixed protocol filters to their real receive window', async () => {
		const { filter } = await load('drm', 12000);
		filter.bindSdrFilter({ onCommit: vi.fn(), onView: vi.fn() });
		expect((document.getElementById('input-sdr-ifbw') as HTMLInputElement).disabled).toBe(true);
		expect(document.querySelector<HTMLButtonElement>('[data-sdr-ifbw="12000"]')!.disabled).toBe(true);
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		expect(input.dataset.state).toBe('locked');
		expect(input.title).toContain('−6');
		expect(input.title).toContain('+6');
	});

	it('preserves a focused draft across STATUS and cancels it with Escape without a blur commit', async () => {
		const { state, filter } = await load();
		const onCommit = vi.fn((hz: number) => state.sdrIfbw.set(hz));
		filter.bindSdrFilter({ onCommit, onView: vi.fn() });
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		input.focus();
		input.value = '7.25';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		state.sdrIfbw.confirm(9000);
		filter.renderSdrFilter();
		expect(input.value).toBe('7.25');
		const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
		input.dispatchEvent(escape);
		input.dispatchEvent(new FocusEvent('blur'));
		expect(escape.defaultPrevented).toBe(true);
		expect(onCommit).not.toHaveBeenCalled();
		expect(input.value).toBe('9');
		expect(state.sdrIfbw.get()).toBe(9000);

		input.focus();
		filter.setFilterPreview(8500);
		expect(input.value).toBe('8.5');
		const previewEscape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
		input.dispatchEvent(previewEscape);
		filter.setFilterPreview(null);
		expect(previewEscape.defaultPrevented).toBe(true);
		expect(onCommit).not.toHaveBeenCalled();
		expect(state.sdrIfbw.get()).toBe(9000);
		expect(input.value).toBe('9');
	});

	it('commits Enter and its following blur once with the value in Hz', async () => {
		const { state, filter } = await load();
		const onCommit = vi.fn((hz: number) => state.sdrIfbw.set(hz));
		filter.bindSdrFilter({ onCommit, onView: vi.fn() });
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		input.focus();
		input.value = '7.25';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		const enter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
		input.dispatchEvent(enter);
		input.blur();
		expect(enter.defaultPrevented).toBe(true);
		expect(onCommit).toHaveBeenCalledExactlyOnceWith(7250);
		expect(state.sdrIfbw.desiredValue()).toBe(7250);
	});

	it('restores a focused clean commit after rejection but keeps a newer uncommitted draft', async () => {
		const { state, filter } = await load();
		filter.bindSdrFilter({ onCommit: (hz) => state.sdrIfbw.set(hz), onView: vi.fn() });
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		input.focus();
		input.value = '4.5';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
		expect(state.sdrIfbw.desiredValue()).toBe(4500);
		state.sdrIfbw.reset();
		filter.renderSdrFilter();
		expect(input.value).toBe('6');
		input.value = '7.25';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		state.sdrIfbw.reset();
		filter.renderSdrFilter();
		expect(input.value).toBe('7.25');
	});

	it('keeps a focused preset as the same DOM node while STATUS updates its active state', async () => {
		const { state, filter } = await load();
		filter.bindSdrFilter({ onCommit: (hz) => state.sdrIfbw.set(hz), onView: vi.fn() });
		const preset = document.querySelector<HTMLButtonElement>('[data-sdr-ifbw="4500"]')!;
		preset.focus();
		state.sdrIfbw.confirm(6000);
		filter.renderSdrFilter();
		expect(document.querySelector('[data-sdr-ifbw="4500"]')).toBe(preset);
		expect(document.activeElement).toBe(preset);
		expect(preset.getAttribute('aria-pressed')).toBe('false');
		state.sdrIfbw.confirm(4500);
		filter.renderSdrFilter();
		expect(document.querySelector('[data-sdr-ifbw="4500"]')).toBe(preset);
		expect(document.activeElement).toBe(preset);
		expect(preset.getAttribute('aria-pressed')).toBe('true');
	});

	it('accepts a custom width beyond mode presets and rejects the published system boundary locally', async () => {
		const { state, filter } = await load();
		state.sdrFilterLimits.confirm({ mode: 'am', min_hz: 50, max_hz: 200_000 });
		const onCommit = vi.fn((hz: number) => state.sdrIfbw.set(hz));
		filter.bindSdrFilter({ onCommit, onView: vi.fn() });
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		input.focus(); input.value = '100';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
		expect(state.sdrIfbw.get()).toBe(100_000);
		expect(input.max).toBe('200');
		input.value = '201'; input.dispatchEvent(new Event('input', { bubbles: true }));
		input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
		expect(onCommit).toHaveBeenCalledOnce();
		expect(state.sdrIfbw.get()).toBe(100_000);
		expect(input.getAttribute('aria-invalid')).toBe('true');
		expect(input.value).toBe('201');
	});

	it('changes display units without a command and submits an edited value in the selected unit once', async () => {
		const { state, filter } = await load();
		// Each case reloads unit slots together with the Filter module.
		const { buildUnitGroups, setUnit } = await import('../core/units');
		buildUnitGroups();
		const onCommit = vi.fn((hz: number) => state.sdrIfbw.set(hz));
		filter.bindSdrFilter({ onCommit, onView: vi.fn() });
		const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
		setUnit('sdr_ifbw', 'Hz');
		expect(input.value).toBe('6000');
		expect(onCommit).not.toHaveBeenCalled();
		input.focus(); input.value = '0.1';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		setUnit('sdr_ifbw', 'MHz');
		input.blur();
		expect(onCommit).toHaveBeenCalledExactlyOnceWith(100_000);
		expect(state.sdrIfbw.get()).toBe(100_000);
	});
});

describe('confirmed Filter memory', () => {
	it('uses first-mode recommendations and stores only valid editable confirmations', async () => {
		const { state, filter } = await load();
		expect(filter.filterValueForMode('cw')).toBe(500);
		expect(filter.filterValueForMode('ft8')).toBe(3000);
		expect(filter.filterValueForMode('wfm')).toBe(180000);
		filter.rememberConfirmedFilter('cw', 900);
		expect(filter.filterValueForMode('cw')).toBe(900);
		expect(JSON.parse(localStorage.getItem('web-sa-sdr-filter-by-mode')!)).toEqual({ cw: 900 });
		for (const invalid of [49, 0, -500, NaN, Infinity]) {
			filter.rememberConfirmedFilter('cw', invalid);
			expect(filter.filterValueForMode('cw')).toBe(900);
		}
		filter.rememberConfirmedFilter('ft8', 6000);
		expect(filter.filterValueForMode('ft8')).toBe(3000);
		expect(state.sdrFilterByMode.get()).toEqual({ cw: 900 });
	});

	it.each([
		['below the mode minimum', '{"cw":49}'],
		['with a non-numeric value', '{"cw":"900"}'],
		['with malformed JSON', '{'],
	])('falls back to the recommendation for saved memory %s', async (_label, saved) => {
		localStorage.setItem('web-sa-sdr-filter-by-mode', saved);
		const { filter } = await load();
		expect(filter.filterValueForMode('cw')).toBe(500);
	});
});
