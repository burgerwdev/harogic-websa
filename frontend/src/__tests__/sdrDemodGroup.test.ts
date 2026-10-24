import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initSdrDemodGroup, refreshSdrDemodGroup, renderSdrDemodGroup } from '../ui/sdrDemodGroup';
import { setPluginManifest, setSstvModeTable, type PluginInfo } from '../sdr/registry';

const plugins: PluginInfo[] = [
	...['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm'].map((id) => ({
		kind: 'analog' as const, id, implemented: id !== 'pm', audioEnhancement: false,
	})),
	...['ft8', 'drm', 'drmplus', 'sstv'].map((id) => ({
		kind: 'digital' as const, id, implemented: true, audioEnhancement: false,
	})),
	{ kind: 'audio', id: 'dc_block', implemented: true, audioEnhancement: true },
];

beforeEach(() => {
	setPluginManifest(null);
	setSstvModeTable(null);
});
afterEach(() => {
	setPluginManifest(null);
	setSstvModeTable(null);
	vi.unstubAllGlobals();
});

const serveMetadata = () => {
	vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
		ok: true,
		json: async () => ({ plugins, sstv_modes: [] }),
	}));
};

describe('the SDR demodulator group', () => {
	it('offers the receiver analog and digital modes, excluding audio stages', async () => {
		serveMetadata();
		const container = document.createElement('div');
		const selected: string[] = [];
		const ids = await initSdrDemodGroup(container, { onSelect: (id) => selected.push(id) });
		expect(ids).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm', 'ft8', 'drm', 'drmplus', 'sstv']);
		const buttons = Array.from(container.querySelectorAll('button'));
		expect(buttons.map((button) => button.dataset.sdrDemod)).toEqual(ids);
		const drmplus = buttons.find((button) => button.dataset.sdrDemod === 'drmplus')!;
		expect(drmplus.textContent).toBe('DRM+');
		drmplus.click();
		expect(selected).toEqual(['drmplus']);
	});

	it('prevents selecting an unavailable mode and refreshes when capabilities change', async () => {
		serveMetadata();
		const container = document.createElement('div');
		const onSelect = vi.fn();
		await initSdrDemodGroup(container, { onSelect });
		const unavailable = container.querySelector<HTMLButtonElement>('[data-sdr-demod="pm"]')!;
		expect(unavailable.disabled).toBe(true);
		expect(unavailable.title).toMatch(/not implemented/);
		unavailable.click();
		expect(onSelect).not.toHaveBeenCalled();

		setPluginManifest(plugins.map((plugin) => ({ ...plugin, implemented: true })));
		refreshSdrDemodGroup(container, { onSelect });
		const enabled = container.querySelector<HTMLButtonElement>('[data-sdr-demod="pm"]')!;
		expect(enabled.disabled).toBe(false);
		enabled.click();
		expect(onSelect).toHaveBeenCalledWith('pm');
	});

	it('keeps undeclared mode ids unavailable', () => {
		setPluginManifest(plugins);
		const container = document.createElement('div');
		const onSelect = vi.fn();
		const [button] = renderSdrDemodGroup(container, ['not_a_mode'], { onSelect });
		expect(button.disabled).toBe(true);
		button.click();
		expect(onSelect).not.toHaveBeenCalled();
	});

	it('shows metadata failure and can load the group after the receiver recovers', async () => {
		const fetchMetadata = vi.fn()
			.mockResolvedValueOnce({ ok: false, status: 503 })
			.mockResolvedValueOnce({ ok: true, json: async () => ({ plugins, sstv_modes: [] }) });
		vi.stubGlobal('fetch', fetchMetadata);
		const container = document.createElement('div');
		const onSelect = vi.fn();
		expect(await initSdrDemodGroup(container, { onSelect })).toEqual([]);
		expect(container.textContent).toBe('Receiver modes unavailable');
		expect(container.querySelector('button')).toBeNull();
		await initSdrDemodGroup(container, { onSelect });
		expect(container.querySelector<HTMLButtonElement>('[data-sdr-demod="am"]')?.disabled).toBe(false);
	});
});
