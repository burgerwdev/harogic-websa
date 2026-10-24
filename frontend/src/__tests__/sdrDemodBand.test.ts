import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	SSTV_AUDIO_HIGH_HZ, SSTV_AUDIO_LOW_HZ, SSTV_BLACK_HZ, SSTV_SYNC_HZ, SSTV_WHITE_HZ,
} from '../sdr/sstvLog';
import { clearDrm, setDrmDecode } from '../sdr/drmLog';
import { audioCompanionFor, loadPluginManifest, setPluginManifest, setSstvModeTable } from '../sdr/registry';
import { demodBandHz } from '../ui/sdrState';

beforeEach(async () => {
	setPluginManifest(null);
	setSstvModeTable(null);
	clearDrm();
	vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
		ok: true,
		json: async () => ({
			plugins: [
				...['usb', 'wfm'].map((id) => ({
					kind: 'analog', id, implemented: true, audioEnhancement: false,
				})),
				...['ft8', 'drm', 'drmplus', 'sstv'].map((id) => ({
					kind: 'digital', id, implemented: true, audioEnhancement: false,
				})),
			],
			sstv_modes: [{ index: 0, name: 'Robot 36', vis: 8, width: 320, height: 240 }],
		}),
	}));
	await loadPluginManifest();
});
afterEach(() => {
	clearDrm();
	setPluginManifest(null);
	setSstvModeTable(null);
	vi.unstubAllGlobals();
});

describe('the band the SDR view highlights', () => {
	it('follows native sideband carrier guards and signed filter edges', () => {
		expect(demodBandHz('usb', 2400)).toEqual([240, 2400]);
		expect(demodBandHz('lsb', 2400)).toEqual([-2400, -240]);
		expect(demodBandHz('usb', 500)).toEqual([100, 500]);
		expect(demodBandHz('lsb', 6000)).toEqual([-6000, -300]);
		expect(demodBandHz('wfm', 180000)).toEqual([-90000, 90000]);
	});

	it('centers CW on the native pitch and preserves the native minimums', () => {
		expect(demodBandHz('cw', 3000, 900)).toEqual([-600, 2400]);
		expect(demodBandHz('cw', 500, 600)).toEqual([350, 850]);
		expect(demodBandHz('cw', 20, 0)).toEqual([75, 125]);
	});

	it('keeps the FT8 search band above the dial regardless of the IF bandwidth', () => {
		expect(demodBandHz('ft8', 3000)).toEqual([100, 3000]);
		expect(demodBandHz('ft8', 6000)).toEqual([100, 3000]);
	});

	it('follows the FAC-selected DRM occupancy independently of the analog filter', () => {
		expect(demodBandHz('drm', 6000)).toEqual([-10000, 10000]);
		setDrmDecode(['locked: B, 4.5 kHz'], 18);
		expect(demodBandHz('drm', 12000)).toEqual([-2250, 2250]);
		setDrmDecode(['locked: B, 20 kHz'], 18);
		expect(demodBandHz('drm', 12000)).toEqual([-10000, 10000]);
		expect(demodBandHz('drmplus', 100000)).toEqual([-50000, 50000]);
	});

	it('keeps the SSTV voice band and picture tones above the dial', () => {
		expect(demodBandHz('sstv', 2400)).toEqual([300, 2700]);
		expect(demodBandHz('sstv', 50000)).toEqual([300, 2700]);
		for (const hz of [SSTV_SYNC_HZ, SSTV_BLACK_HZ, SSTV_WHITE_HZ]) {
			expect(hz).toBeGreaterThan(SSTV_AUDIO_LOW_HZ);
			expect(hz).toBeLessThan(SSTV_AUDIO_HIGH_HZ);
		}
		expect(audioCompanionFor('sstv')).toBe('usb');
	});
});
