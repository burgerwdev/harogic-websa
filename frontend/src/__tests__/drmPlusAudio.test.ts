import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const artifact = resolve(process.cwd(), 'public/dsp.wasm');
const fixture = resolve(process.cwd(), '../tests/fixtures/drm/drm_modeE_so0_96k_aac.f32');

// Independently synthesised mode E IQ: the native receiver asserts FAC/SDC CRC,
// 4-frame/depth-6 MSC bit identity and the paired AAC access units before writing it.
describe('mode E DRM+ audio through the shipped wasm ABI', () => {
	for (const outputRate of [48_000, 44_100]) it(`decodes PCM with ${outputRate} Hz sound output`, async () => {
		const raw = readFileSync(artifact);
		const dsp = await instantiateDsp(raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength));
		const mode = new TextEncoder().encode('drmplus');
		const modePtr = dsp.alloc(mode.length);
		dsp.u8View(modePtr, mode.length).set(mode);
		const handle = dsp.exports.websa_dsp_demod_new(96_000, outputRate, modePtr, mode.length, 100_000, 0);
		dsp.free(modePtr, mode.length);
		expect(handle).toBeGreaterThan(0);

		const data = readFileSync(fixture);
		const iq = new Float32Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength));
		const chunk = 3248;
		const inputPtr = dsp.alloc(chunk * 2 * 4);
		for (let pos = 0; pos < iq.length / 2; pos += chunk) {
			const n = Math.min(chunk, iq.length / 2 - pos);
			dsp.f32View(inputPtr, n * 2).set(iq.subarray(pos * 2, (pos + n) * 2));
			dsp.exports.websa_dsp_demod_push(handle, inputPtr, n);
		}
		expect(dsp.exports.websa_dsp_demod_count(handle)).toBeGreaterThan(5);
		expect(dsp.exports.websa_dsp_drm_audio_rate(handle)).toBe(24_000);
		const pcmPtr = dsp.alloc(100_000 * 2);
		const samples = dsp.exports.websa_dsp_drm_audio_pcm(handle, pcmPtr, 100_000);
		expect(samples).toBeGreaterThan(5_000);
		const pcm = dsp.i16View(pcmPtr, samples);
		let peak = 0;
		let energy = 0;
		for (const s of pcm) { peak = Math.max(peak, Math.abs(s)); energy += Math.abs(s); }
		expect(peak).toBeGreaterThan(1000);
		expect(energy / samples).toBeGreaterThan(100);
		dsp.free(inputPtr, chunk * 2 * 4);
		dsp.free(pcmPtr, 100_000 * 2);
		dsp.exports.websa_dsp_demod_free(handle);
	});
});
