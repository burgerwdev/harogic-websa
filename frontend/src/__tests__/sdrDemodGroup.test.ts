import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initSdrDemodGroup, refreshSdrDemodGroup, renderSdrDemodGroup, syncSdrDemodGroup } from '../ui/sdrDemodGroup';
import { setPluginManifest, setSstvModeTable, type PluginInfo } from '../sdr/registry';
import { receiverPlugin } from './pluginFixtures';

const plugins: PluginInfo[] = [
	receiverPlugin('am'), receiverPlugin('usb'),
	receiverPlugin('ais', { common: false, label: 'Automatic Identification System', category: 'maritime', aliases: ['ships'], provider: 'onair' }),
	receiverPlugin('ism', { common: false, category: 'ism', available: false, unavailableReason: 'rtl_433 is not installed' }),
	{ kind: 'audio', id: 'dc_block', implemented: true, audioEnhancement: true },
];

beforeEach(() => { setPluginManifest(null); setSstvModeTable(null); });
afterEach(() => {
	setPluginManifest(null); setSstvModeTable(null); vi.unstubAllGlobals(); document.body.replaceChildren();
});

function serveMetadata(): void {
	vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ plugins, sstv_modes: [] }) }));
}
function host(): HTMLElement {
	const container = document.createElement('div');
	document.body.append(container);
	return container;
}

describe('the receiver mode catalog', () => {
	it('shows common modes and keeps a selected non-common mode visible', async () => {
		serveMetadata();
		const container = host();
		let selected = 'am';
		const hooks = { onSelect: vi.fn(), selected: () => selected };
		expect(await initSdrDemodGroup(container, hooks)).toEqual(['am', 'usb', 'ais', 'ism']);
		expect([...container.querySelectorAll('.sdr-mode-common button')].map((el) => (el as HTMLElement).dataset.sdrDemod)).toEqual(['am', 'usb', 'ais']);
		selected = 'ism';
		syncSdrDemodGroup(container, selected);
		expect(container.querySelector('.sdr-mode-common [data-sdr-demod="ism"]')?.getAttribute('aria-pressed')).toBe('true');
		expect(container.querySelector('[data-sdr-demod="dc_block"]')).toBeNull();
	});

	it('exposes the requested common badges while retaining dependency gating', async () => {
		const promoted = ['drm', 'ais', 'adsb', 'aprs', 'wspr', 'ft4', 'radiofax', 'rtty'];
		const reason = 'RTTY decoder is not installed';
		setPluginManifest([
			receiverPlugin('am'), receiverPlugin('usb'),
			...promoted.map((id) => receiverPlugin(id, {
				kind: 'digital', common: false, available: id !== 'rtty', unavailableReason: id === 'rtty' ? reason : null,
			})),
			receiverPlugin('rtty-reverse', { kind: 'digital', common: false }),
		]);
		const container = host();
		const onSelect = vi.fn();
		await initSdrDemodGroup(container, { onSelect });
		const buttons = [...container.querySelectorAll<HTMLButtonElement>('.sdr-mode-common button')];
		expect(buttons.map((button) => button.dataset.sdrDemod)).toEqual(['am', 'usb', ...promoted]);
		expect(buttons.map((button) => button.textContent)).toEqual(['AM', 'USB', 'DRM', 'AIS', 'ADS-B', 'APRS', 'WSPR', 'FT4', 'FAX', 'RTTY']);
		for (const id of promoted) {
			const button = buttons.find((candidate) => candidate.dataset.sdrDemod === id)!;
			expect(button.disabled).toBe(id === 'rtty');
			button.click();
			if (id === 'rtty') {
				expect(button.title).toBe(reason);
				expect(onSelect).not.toHaveBeenCalledWith(id);
			} else expect(onSelect).toHaveBeenCalledWith(id);
		}
	});

	it('searches aliases and selects with the same callback from the keyboard', async () => {
		serveMetadata();
		const container = host();
		const onSelect = vi.fn();
		await initSdrDemodGroup(container, { onSelect });
		const search = container.querySelector<HTMLInputElement>('.sdr-mode-tools input')!;
		search.focus(); search.value = 'ships'; search.dispatchEvent(new Event('input'));
		expect(container.querySelectorAll('.sdr-mode-quick [data-sdr-demod]')).toHaveLength(1);
		search.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
		expect(onSelect).toHaveBeenCalledExactlyOnceWith('ais');
		expect(container.querySelector<HTMLElement>('.sdr-mode-quick')!.hidden).toBe(true);
	});

	it('filters drawer categories, shows dependency reasons and restores focus on Escape', async () => {
		serveMetadata();
		const container = host();
		const onSelect = vi.fn();
		await initSdrDemodGroup(container, { onSelect });
		const more = container.querySelector<HTMLButtonElement>('[aria-haspopup="dialog"]')!;
		more.click();
		const dialog = container.querySelector('dialog')!;
		expect(dialog.open).toBe(true);
		const category = dialog.querySelector('select')!;
		category.value = 'ism'; category.dispatchEvent(new Event('change'));
		const unavailable = dialog.querySelector<HTMLButtonElement>('[data-sdr-demod="ism"]')!;
		expect(unavailable.disabled).toBe(true);
		expect(unavailable.textContent).toContain('rtl_433 is not installed');
		expect(dialog.querySelector('[data-sdr-demod="ais"]')).toBeNull();
		unavailable.click(); expect(onSelect).not.toHaveBeenCalled();
		dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
		expect(dialog.open).toBe(false);
		expect(document.activeElement).toBe(more);
	});

	it('dismisses on the backdrop but keeps drawer whitespace clicks open', async () => {
		serveMetadata();
		const container = host();
		await initSdrDemodGroup(container, { onSelect: vi.fn() });
		const more = container.querySelector<HTMLButtonElement>('[aria-haspopup="dialog"]')!;
		more.click();
		const dialog = container.querySelector('dialog')!;
		vi.spyOn(dialog, 'getBoundingClientRect').mockReturnValue({
			x: 600, y: 0, left: 600, top: 0, right: 1000, bottom: 800, width: 400, height: 800, toJSON: () => ({}),
		});
		dialog.dispatchEvent(new MouseEvent('click', { bubbles: true, clientX: 610, clientY: 400 }));
		expect(dialog.open).toBe(true);
		dialog.dispatchEvent(new MouseEvent('click', { bubbles: true, clientX: 500, clientY: 400 }));
		expect(dialog.open).toBe(false);
		expect(document.activeElement).toBe(more);
	});

	it('keeps read-only browsing open across status synchronization', async () => {
		serveMetadata();
		const container = host();
		const onSelect = vi.fn();
		await initSdrDemodGroup(container, { onSelect, canSelect: () => false });
		const more = container.querySelector<HTMLButtonElement>('[aria-haspopup="dialog"]')!;
		more.click();
		const dialog = container.querySelector('dialog')!;
		const search = dialog.querySelector<HTMLInputElement>('input[type="search"]')!;
		search.value = 'ships';
		search.dispatchEvent(new Event('input'));
		search.focus();
		syncSdrDemodGroup(container, 'am');
		expect(dialog.open).toBe(true);
		expect(more.getAttribute('aria-expanded')).toBe('true');
		expect(document.activeElement).toBe(search);
		expect(search.value).toBe('ships');
		const ais = dialog.querySelector<HTMLButtonElement>('[data-sdr-demod="ais"]')!;
		expect(ais.disabled).toBe(true);
		ais.click();
		expect(onSelect).not.toHaveBeenCalled();
	});

	it('rechecks live permissions and refreshes resolved availability', async () => {
		serveMetadata();
		const container = host();
		const onSelect = vi.fn();
		let allowed = true;
		const hooks = { onSelect, canSelect: () => allowed };
		await initSdrDemodGroup(container, hooks);
		const am = container.querySelector<HTMLButtonElement>('[data-sdr-demod="am"]')!;
		allowed = false; am.click(); expect(onSelect).not.toHaveBeenCalled();
		syncSdrDemodGroup(container, 'am');
		expect(container.querySelector<HTMLButtonElement>('.sdr-mode-common [data-sdr-demod="am"]')!.disabled).toBe(true);
		allowed = true;
		setPluginManifest(plugins.map((plugin) => plugin.id === 'ism' ? { ...plugin, available: true, common: true } : plugin));
		refreshSdrDemodGroup(container, hooks);
		const ism = container.querySelector<HTMLButtonElement>('.sdr-mode-common [data-sdr-demod="ism"]')!;
		expect(ism.disabled).toBe(false); ism.click(); expect(onSelect).toHaveBeenCalledWith('ism');
	});

	it('keeps undeclared mode ids unavailable', () => {
		setPluginManifest(plugins);
		const onSelect = vi.fn();
		const [button] = renderSdrDemodGroup(host(), ['not_a_mode'], { onSelect });
		expect(button.disabled).toBe(true); button.click(); expect(onSelect).not.toHaveBeenCalled();
	});

	it('can retry after metadata failure without publishing a partial catalog', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: false, status: 503 })
			.mockResolvedValueOnce({ ok: true, json: async () => ({ plugins, sstv_modes: [] }) }));
		const container = host();
		const hooks = { onSelect: vi.fn() };
		expect(await initSdrDemodGroup(container, hooks)).toEqual([]);
		expect(container.textContent).toBe('Receiver modes unavailable');
		await initSdrDemodGroup(container, hooks);
		expect(container.querySelector<HTMLButtonElement>('[data-sdr-demod="am"]')?.disabled).toBe(false);
	});
});
