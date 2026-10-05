/**
 * DRM codec end-to-end through the shipped wasm path: the receiver (not a standalone
 * decoder) must turn a live bench capture into PCM for xHE-AAC and HE-AAC v2, the two
 * codings the committed bench fixtures did not cover. Each fixture is a real DecDRM
 * transmission captured from the Pluto -> SAN-90 bench loop at the channelizer's rate,
 * fed in worker-sized blocks like the DSP worker does.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const DIR = resolve(process.cwd(), '..', 'tests', 'fixtures', 'drm');
const BLOCK = 3248;

interface Decoded {
	rate: number;
	samples: number;
	peak: number;
	meanAbs: number;
	toneFraction: number;
}

async function decode(file: string): Promise<Decoded> {
	const buf = readFileSync(ARTIFACT);
	const dsp = await instantiateDsp(buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength));
	const mode = new TextEncoder().encode('drm');
	const modePtr = dsp.alloc(mode.length);
	dsp.u8View(modePtr, mode.length).set(mode);
	const handle = dsp.exports.websa_dsp_demod_new(48_828, 48_000, modePtr, mode.length, 10_000, 0);
	dsp.free(modePtr, mode.length);
	expect(handle).toBeGreaterThan(0);

	const raw = readFileSync(resolve(DIR, file));
	const iq = new Float32Array(raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength));
	const complex = Math.floor(iq.length / 2);
	const blockPtr = dsp.alloc(BLOCK * 2 * 4);
	for (let off = 0; off < complex; off += BLOCK) {
		const n = Math.min(BLOCK, complex - off);
		dsp.f32View(blockPtr, n * 2).set(iq.subarray(off * 2, off * 2 + n * 2));
		dsp.exports.websa_dsp_demod_push(handle, blockPtr, n);
	}
	dsp.free(blockPtr, BLOCK * 2 * 4);

	const rate = dsp.exports.websa_dsp_drm_audio_rate(handle);
	const pcmCap = 48_000 * 30;
	const pcmPtr = dsp.alloc(pcmCap * 2);
	const samples = dsp.exports.websa_dsp_drm_audio_pcm(handle, pcmPtr, pcmCap);
	const pcm = dsp.i16View(pcmPtr, samples);
	let peak = 0;
	let energy = 0;
	for (let i = 0; i < samples; i++) {
		const v = Math.abs(pcm[i]);
		peak = Math.max(peak, v);
		energy += v;
	}
	// The PCM ABI feeds a mono worklet. HE-AAC v2 is decoded in stereo and downmixed before
	// leaving wasm; the 1 kHz tone must still occupy one second of 24 kHz PCM here.
	const n = Math.min(rate, samples);
	const energyIn = (hz: number): number => {
		const w = (2 * Math.PI * hz) / rate;
		let re = 0;
		let im = 0;
		for (let i = 0; i < n; i++) {
			const v = pcm[i];
			re += v * Math.cos(w * i);
			im += v * Math.sin(w * i);
		}
		return Math.hypot(re, im);
	};
	let tone = energyIn(1000);
	let other = 0;
	for (let hz = 200; hz <= 3000; hz += 50) {
		if (Math.abs(hz - 1000) < 60) continue;
		other = Math.max(other, energyIn(hz));
	}
	return { rate, samples, peak, meanAbs: samples ? energy / samples : 0, toneFraction: tone / Math.max(other, 1) };
}

describe('DRM codec coverage (xHE-AAC, HE-AAC v2) through the receiver', () => {
	it('decodes a live xHE-AAC (coding 3) capture to non-silent PCM', async () => {
		const d = await decode('drm_live_xhe_modeB_so3_48828.f32');
		expect(d.rate).toBe(24_000);
		expect(d.samples).toBeGreaterThan(50_000);
		expect(d.peak).toBeGreaterThan(1000);
		expect(d.meanAbs).toBeGreaterThan(100);
		// The 1 kHz bench tone dominates.
		expect(d.toneFraction).toBeGreaterThan(5);
	});

	it('decodes a live HE-AAC v2 (SBR + parametric stereo) capture to mono playback PCM', async () => {
		const d = await decode('drm_live_heaacv2_modeB_so3_48828.f32');
		expect(d.rate).toBe(24_000);
		// At 24 kHz the one-channel worklet receives frames, not interleaved stereo samples.
		expect(d.samples).toBeGreaterThan(50_000);
		expect(d.samples).toBeLessThan(100_000);
		expect(d.peak).toBeGreaterThan(1000);
		expect(d.meanAbs).toBeGreaterThan(100);
		expect(d.toneFraction).toBeGreaterThan(5);
	});
});
