/**
 * DRM audio end-to-end test: the committed real-audio fixture's MSC carries a 24 kHz
 * AAC audio super frame, so the full receive chain (OFDM → FAC/SDC/MSC → deframe →
 * AAC decode) must surface non-silent PCM through the `websa_dsp_drm_audio_pcm` ABI.
 *
 * This is the in-repo counterpart of the native `fixture_deframes_real_aac_audio` test,
 * which only verifies the deframe step (the AAC decoder is wasm32-only).
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', 'tests', 'fixtures', 'drm', 'drm_modeB_so3_48k_aac.f32');
/// 6 s of interleaved f32 I/Q at 48 kHz => 288000 complex samples.
const COMPLEX = 288000;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('DRM audio end-to-end (IQ → AAC PCM)', () => {
	it('decodes non-silent AAC audio through the receive chain', async () => {
		const dsp = await instantiateDsp(artifactBytes());

		// Create a DRM digital pipeline.
		const mode = new TextEncoder().encode('drm');
		const modePtr = dsp.alloc(mode.length);
		dsp.u8View(modePtr, mode.length).set(mode);
		const handle = dsp.exports.websa_dsp_demod_new(48000, 48000, modePtr, mode.length, 10000, 0);
		dsp.free(modePtr, mode.length);
		expect(handle).toBeGreaterThan(0);

		// Push the whole fixture (one call; MAX_PUSH_SAMPLES is 1<<20 complex samples).
		const iq = readFileSync(FIXTURE);
		expect(iq.length / 4).toBe(COMPLEX * 2);
		const iqPtr = dsp.alloc(iq.length);
		dsp.f32View(iqPtr, COMPLEX * 2).set(
			new Float32Array(iq.buffer.slice(iq.byteOffset, iq.byteOffset + iq.byteLength)),
		);
		const count = dsp.exports.websa_dsp_demod_push(handle, iqPtr, COMPLEX);
		expect(count).toBeGreaterThan(0);

		// The decoded AAC core runs at 24 kHz; the receiver deframes + decodes it to PCM.
		expect(dsp.exports.websa_dsp_drm_audio_rate(handle)).toBe(24000);

		const pcmPtr = dsp.alloc(200000 * 2);
		const samples = dsp.exports.websa_dsp_drm_audio_pcm(handle, pcmPtr, 200000);
		expect(samples).toBeGreaterThan(0);

		const pcm = dsp.i16View(pcmPtr, samples);
		let energy = 0;
		for (let i = 0; i < samples; i++) energy += pcm[i] < 0 ? -pcm[i] : pcm[i];
		expect(energy).toBeGreaterThan(0);

		dsp.free(iqPtr, iq.length);
		dsp.free(pcmPtr, 200000 * 2);
		dsp.exports.websa_dsp_demod_free(handle);
	});
});
