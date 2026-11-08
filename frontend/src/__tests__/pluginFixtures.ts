import type { ModeDescriptor } from '../sdr/registry';

/** Explicit catalog fixtures shared by receiver interaction tests. */
export function receiverPlugin(id: string, overrides: Partial<ModeDescriptor> = {}): ModeDescriptor {
	const digital = ['ft8', 'drm', 'drmplus', 'sstv', 'radiofax', 'ais', 'ism'].includes(id);
	const defaultHz = id === 'wfm' ? 180_000 : id === 'cw' ? 500 : id === 'usb' || id === 'lsb' ? 2_400
		: id === 'nfm' || id === 'pm' || id === 'drm' ? 12_000 : id === 'drmplus' ? 100_000 : digital ? 3_000 : 6_000;
	const minimumHz = id === 'usb' || id === 'lsb' ? 101 : 50;
	const presets = id === 'am' || id === 'dsb' ? [3_000, 4_500, 6_000, 9_000, 12_000]
		: id === 'usb' || id === 'lsb' ? [1_800, 2_400, 2_700, 3_000, 6_000]
		: id === 'cw' ? [100, 200, 500, 1_000, 2_000]
		: id === 'nfm' ? [9_000, 12_000, 12_500, 15_000, 25_000]
		: id === 'pm' ? [6_000, 9_000, 12_000, 15_000, 25_000]
		: id === 'wfm' ? [100_000, 150_000, 180_000, 200_000, 250_000] : [defaultHz];
	return {
		id, kind: digital ? 'digital' : 'analog', implemented: true, available: true, audioEnhancement: false,
		label: id.toUpperCase(), category: digital ? 'amateur' : 'analog', description: '', aliases: [], provider: 'native',
		startupTimeoutMs: 3000,
		unavailableReason: null, defaultBandwidthHz: defaultHz, minBandwidthHz: minimumHz, maxBandwidthHz: 1_000_000,
		bandwidthPresetsHz: presets, bandwidthEditable: !digital, inputRateHz: null, minimumRateHz: 0,
		audioCompanion: digital && id !== 'drm' && id !== 'drmplus' ? 'usb' : null,
		sideband: id === 'lsb' ? 'lower' : id === 'usb' || ['ft8', 'sstv', 'radiofax'].includes(id) ? 'upper' : 'symmetric',
		outputs: digital ? ['text'] : ['audio'], common: true, ...overrides,
	};
}
