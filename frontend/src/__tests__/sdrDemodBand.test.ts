/**
 * The band the SDR view highlights must be the band the active demodulator actually reads.
 *
 * A protocol decoder is the case that differs: FT8 reads 100..3000 Hz *above* the dial, not a
 * passband centred on it. Highlighting a symmetric passband instead claimed half a band the decoder
 * cannot read and hid half of the one it can, which reads as "the signal is inside the marked band
 * and still does not decode". Measured on the bench (Pluto at 411.0015 MHz, dial parked on the tone):
 * 0 decodes in 5 slots while the overlay covered the signal.
 *
 * The last case is the guard that would have caught the drift this replaced: the TS band said 200 Hz
 * while the decoder said 100, and nothing tied them together.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { FT8_SEARCH_HIGH_HZ, FT8_SEARCH_LOW_HZ } from '../sdr/ft8Log';
import { readPluginManifest, setPluginManifest } from '../sdr/registry';
import { instantiateDsp } from '../sdr/wasm';
import { demodBandHz } from '../ui/sdrState';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FT8_RS = resolve(process.cwd(), '..', 'wasm', 'src', 'digital', 'ft8', 'mod.rs');

async function loadManifest(): Promise<void> {
	const buf = readFileSync(ARTIFACT);
	const bytes = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
	setPluginManifest(await readPluginManifest(await instantiateDsp(bytes)));
}

describe('the band the SDR view highlights', () => {
	it('is the IF passband, symmetric about the listen frequency, for an analog demodulator', () => {
		expect(demodBandHz('usb', 2400)).toEqual([-1200, 1200]);
		expect(demodBandHz('wfm', 180000)).toEqual([-90000, 90000]);
	});

	it('is the decoder band above the dial, not around it, for a protocol decoder', async () => {
		await loadManifest();
		expect(demodBandHz('ft8', 3000)).toEqual([100, 3000]);
		// The IF bandwidth is not the decoder's band: it must not move it.
		expect(demodBandHz('ft8', 6000)).toEqual([100, 3000]);
	});

	it('highlights the currently decoded DRM30 10 kHz channel, not FT8 or the analog filter', async () => {
		await loadManifest();
		expect(demodBandHz('drm', 6000)).toEqual([-5000, 5000]);
		expect(demodBandHz('drm', 12000)).toEqual([-5000, 5000]);
		expect(demodBandHz('drmplus', 100000)).toEqual([-50000, 50000]);
	});

	it('cannot drift from the decoder that defines it', () => {
		const rust = readFileSync(FT8_RS, 'utf8');
		const declared = (name: string): number => {
			const found = new RegExp(`const ${name}: f64 = ([0-9_]+)`).exec(rust);
			expect(found, `${name} in ${FT8_RS}`).not.toBeNull();
			return Number(found![1].replace(/_/g, ''));
		};
		expect(declared('F_MIN_HZ')).toBe(FT8_SEARCH_LOW_HZ);
		expect(declared('F_MAX_HZ')).toBe(FT8_SEARCH_HIGH_HZ);
	});
});
