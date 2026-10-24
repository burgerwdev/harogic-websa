import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	analogModeIds, audioCompanionFor, audioStageIds, ddcStageIds, digitalModeIds,
	isModeAvailable, loadPluginManifest, pluginManifest, sdrModeIds, setPluginManifest,
	setSstvModeTable, sstvModeTable, type PluginInfo,
} from '../sdr/registry';

const plugins: PluginInfo[] = [
	...['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm'].map((id) => ({
		kind: 'analog' as const, id, implemented: true, audioEnhancement: false,
	})),
	...['ft8', 'drm', 'drmplus', 'sstv'].map((id) => ({
		kind: 'digital' as const, id, implemented: true, audioEnhancement: false,
	})),
	{ kind: 'audio', id: 'dc_block', implemented: true, audioEnhancement: true },
	{ kind: 'ddc', id: 'nco', implemented: true, audioEnhancement: false },
];
const modes = [
	{ index: 0, name: 'Robot 36', vis: 8, width: 320, height: 240 },
	{ index: 1, name: 'Martin M1', vis: 44, width: 320, height: 256 },
];
const metadata = () => ({ plugins: plugins.map((p) => ({ ...p })), sstv_modes: modes.map((m) => ({ ...m })) });
const respond = (body: unknown) => ({ ok: true, json: async () => body });

beforeEach(() => {
	setPluginManifest(null);
	setSstvModeTable(null);
});
afterEach(() => {
	setPluginManifest(null);
	setSstvModeTable(null);
	vi.unstubAllGlobals();
});

describe('native receiver metadata', () => {
	it('shares one request and caches the published metadata', async () => {
		const fetchMetadata = vi.fn().mockResolvedValue(respond(metadata()));
		vi.stubGlobal('fetch', fetchMetadata);
		const [first, second] = await Promise.all([loadPluginManifest(), loadPluginManifest()]);
		expect(fetchMetadata).toHaveBeenCalledExactlyOnceWith('/api/sdr/plugins');
		expect(first).toEqual(plugins);
		expect(second).toBe(first);
		expect(await loadPluginManifest()).toBe(first);
		expect(fetchMetadata).toHaveBeenCalledTimes(1);
		expect(sstvModeTable()).toEqual(modes);
	});


	it('exposes receiver mode families, availability, and audio companions', async () => {
		const body = metadata();
		body.plugins.find((p) => p.id === 'pm')!.implemented = false;
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond(body)));
		await loadPluginManifest();
		expect(analogModeIds()).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm']);
		expect(digitalModeIds()).toEqual(['ft8', 'drm', 'drmplus', 'sstv']);
		expect(sdrModeIds()).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm', 'ft8', 'drm', 'drmplus', 'sstv']);
		expect(audioStageIds()).toEqual(['dc_block']);
		expect(ddcStageIds()).toEqual(['nco']);
		expect(isModeAvailable('am')).toBe(true);
		expect(isModeAvailable('pm')).toBe(false);
		expect(isModeAvailable('unknown')).toBe(false);
		expect(audioCompanionFor('ft8')).toBe('usb');
		expect(audioCompanionFor('sstv')).toBe('usb');
		expect(audioCompanionFor('drm')).toBeNull();
		expect(audioCompanionFor('drmplus')).toBeNull();
	});

	it('retries a failed metadata request without publishing partial state', async () => {
		const fetchMetadata = vi.fn()
			.mockResolvedValueOnce({ ok: false, status: 503 })
			.mockResolvedValueOnce(respond(metadata()));
		vi.stubGlobal('fetch', fetchMetadata);
		await expect(loadPluginManifest()).rejects.toThrow('Receiver metadata: HTTP 503');
		expect(pluginManifest()).toBeNull();
		expect(sstvModeTable()).toBeNull();
		await expect(loadPluginManifest()).resolves.toEqual(plugins);
		expect(fetchMetadata).toHaveBeenCalledTimes(2);
	});

	it.each([
		{ plugins: {}, sstv_modes: [] },
		{ plugins: [{ kind: 'unknown', id: 'am', implemented: true }], sstv_modes: [] },
		{ plugins: [{ kind: 'analog', id: 'am', implemented: 'yes' }], sstv_modes: [] },
		{ plugins, sstv_modes: [{ ...modes[0], index: 1 }] },
		{ plugins, sstv_modes: [{ ...modes[0], width: 0 }] },
	])('rejects invalid receiver metadata atomically: %j', async (body) => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond(body)));
		await expect(loadPluginManifest()).rejects.toThrow(/Invalid receiver/);
		expect(pluginManifest()).toBeNull();
		expect(sstvModeTable()).toBeNull();
	});

	it('offers no modes before receiver capabilities arrive', () => {
		expect(sdrModeIds()).toEqual([]);
		expect(isModeAvailable('am')).toBe(false);
	});
});
