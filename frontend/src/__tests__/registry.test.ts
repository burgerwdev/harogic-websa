import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	analogModeIds, audioCompanionFor, audioStageIds, ddcStageIds, digitalModeIds,
	isModeAvailable, loadPluginManifest, pluginManifest, sdrModeIds, setPluginManifest,
	setSstvModeTable, sstvModeTable, modeDescriptor, modeOutputs, modeStartupTimeoutMs, availableSdrModeIds, type PluginInfo,
} from '../sdr/registry';
import { receiverPlugin } from './pluginFixtures';

const plugins: PluginInfo[] = [
	...['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm', 'ft8', 'drm', 'drmplus', 'sstv'].map((id) => receiverPlugin(id)),
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

describe('receiver metadata', () => {
	it('accepts the same stage ID in separate audio and DDC namespaces', async () => {
		const stages: PluginInfo[] = [
			{ kind: 'audio', id: 'agc', implemented: true, audioEnhancement: true },
			{ kind: 'ddc', id: 'agc', implemented: true, audioEnhancement: false },
		];
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond({ plugins: stages, sstv_modes: [] })));
		await expect(loadPluginManifest()).resolves.toEqual(stages);
		expect(audioStageIds()).toEqual(['agc']);
		expect(ddcStageIds()).toEqual(['agc']);
	});

	it.each([
		[
			{ kind: 'audio', id: 'agc', implemented: true, audioEnhancement: true },
			{ kind: 'audio', id: 'agc', implemented: true, audioEnhancement: true },
		],
		[
			{ kind: 'ddc', id: 'agc', implemented: true, audioEnhancement: false },
			{ kind: 'ddc', id: 'agc', implemented: true, audioEnhancement: false },
		],
		[receiverPlugin('am'), receiverPlugin('am', { kind: 'digital' })],
		[receiverPlugin('am'), receiverPlugin('am')],
	])('rejects duplicate identities within one stage or receiver namespace', async (first, second) => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond({ plugins: [first, second], sstv_modes: [] })));
		await expect(loadPluginManifest()).rejects.toThrow('Duplicate receiver plugin');
		expect(pluginManifest()).toBeNull();
		expect(sstvModeTable()).toBeNull();
	});

	it('retains external requirements and excludes missing dependencies from cycling', async () => {
		const external = receiverPlugin('ais', { common: false, provider: 'onair', startupTimeoutMs: 15000, aliases: ['ships'],
			available: false, unavailableReason: 'OnAir server missing', outputs: ['table'], audioCompanion: null,
			inputRateHz: 96_000, minimumRateHz: 48_000, defaultBandwidthHz: 25_000,
			minBandwidthHz: 25_000, maxBandwidthHz: 25_000, bandwidthPresetsHz: [25_000], bandwidthEditable: false });
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond({ plugins: [receiverPlugin('am'), external], sstv_modes: [] })));
		await loadPluginManifest();
		expect(modeDescriptor('ais')).toEqual(external);
		expect(modeOutputs('ais')).toEqual(['table']);
		expect(availableSdrModeIds()).toEqual(['am']);
		expect(audioCompanionFor('ais')).toBeNull();
		expect(modeStartupTimeoutMs('ais')).toBe(15000);
		expect(modeStartupTimeoutMs('am')).toBe(3000);
	});
	it('accepts GPS table and status output capabilities from the catalog', async () => {
		const gps = receiverPlugin('gps', { kind: 'digital', provider: 'gnss-sdr', category: 'satellite',
			startupTimeoutMs: 25000, outputs: ['table', 'status'], audioCompanion: null });
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond({ plugins: [gps], sstv_modes: [] })));
		await loadPluginManifest();
		expect(modeOutputs('gps')).toEqual(['table', 'status']);
		expect(modeStartupTimeoutMs('gps')).toBe(25000);
		expect(modeDescriptor('gps')).toEqual(gps);
	});
	it.each([undefined, null, 0, -1, 1.5, Infinity, 120001, '25000'])('rejects invalid startup deadlines: %s', async (startupTimeoutMs) => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(respond({ plugins: [{ ...receiverPlugin('am'), startupTimeoutMs }], sstv_modes: [] })));
		await expect(loadPluginManifest()).rejects.toThrow('Invalid receiver plugin');
		expect(pluginManifest()).toBeNull();
	});
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
		Object.assign(body.plugins.find((p) => p.id === 'pm')!, { available: false });
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
		{ plugins: [receiverPlugin('am', { maxBandwidthHz: 1 })], sstv_modes: [] },
		{ plugins: [receiverPlugin('am', { outputs: ['invalid' as never] })], sstv_modes: [] },
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
