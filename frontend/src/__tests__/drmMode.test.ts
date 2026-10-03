// DRM mode wiring in the browser: the mode is offered even though its kernel runs in the backend.
//
// DRM is demodulated by the external Dream process, so the panel must list it without the wasm
// registry declaring an analog/digital plugin, and STATUS's `sdr.drm` must render as a readout.
import { afterEach, describe, expect, it } from 'vitest';
import { isModeAvailable, isServerMode, sdrModeIds, setPluginManifest, type PluginInfo } from '../sdr/registry';
import { demodBandHz } from '../ui/sdrState';
import { renderSdrDemodGroup } from '../ui/sdrDemodGroup';
import { renderDrmStatus } from '../ui/drmStatus';

const manifest: PluginInfo[] = [
	{ kind: 'analog', id: 'am', implemented: true, audioEnhancement: false },
];

afterEach(() => {
	setPluginManifest(null);
	document.body.innerHTML = '';
});

describe('the DRM demodulator', () => {
	it('is offered as an available server-side mode', () => {
		setPluginManifest(manifest);
		expect(sdrModeIds()).toContain('drm');
		expect(isServerMode('drm')).toBe(true);
		expect(isModeAvailable('drm')).toBe(true);
		// A mode neither in the manifest nor a server mode stays unavailable.
		expect(isModeAvailable('not_a_mode')).toBe(false);
	});

	it('highlights the 10 kHz DRM channel band', () => {
		expect(demodBandHz('drm', 6000)).toEqual([-5000, 5000]);
	});

	it('renders an enabled DRM button in the demod group', () => {
		setPluginManifest(manifest);
		const container = document.createElement('div');
		renderSdrDemodGroup(container, sdrModeIds(), { onSelect: () => {} });
		const button = Array.from(container.querySelectorAll('button'))
			.find((b) => (b as HTMLElement).dataset.sdrDemod === 'drm') as HTMLButtonElement;
		expect(button).toBeTruthy();
		expect(button.disabled).toBe(false);
	});

	it('renders decoded station metadata and clears it when inactive', () => {
		document.body.innerHTML =
			'<div id="drm-status-row" style="display:none"></div><span id="cur-drm-status"></span>';
		renderDrmStatus({
			drm: {
				active: true, station: 'SAN90 DRM TEST', robustness: 'B',
				bandwidth_khz: 10, bitrate_kbps: 20.96, audio_codec: 'AAC', sync: true,
			},
		});
		const row = document.getElementById('drm-status-row') as HTMLElement;
		const el = document.getElementById('cur-drm-status') as HTMLElement;
		expect(row.style.display).toBe('');
		expect(el.textContent).toContain('SAN90 DRM TEST');
		expect(el.textContent).toContain('Mode B');
		expect(el.textContent).toContain('10 kHz');
		expect(el.textContent).toContain('21.0 kbps');
		expect(el.textContent).toContain('SYNC');

		renderDrmStatus({ drm: { active: false } });
		expect(row.style.display).toBe('none');
		expect(el.textContent).toBe('');
	});
});
