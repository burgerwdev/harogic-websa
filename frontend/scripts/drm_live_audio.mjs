/**
 * DRM live-capture audio check (wasm, node-local): feeds the committed live bench
 * capture (12 kHz HE-AAC core with SBR, 48828.125 Hz DDC rate) through the full
 * receive chain in 3248-sample blocks, like the worker does, and reports the PCM the
 * `websa_dsp_drm_audio_pcm` ABI surfaces. A diagnostic tool, not a gate — the
 * regression gate is frontend/src/__tests__/drmLiveAudio.test.ts.
 *
 *   node --experimental-strip-types scripts/drm_live_audio.mjs [--whole]
 */
import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { instantiateDsp } from '../src/sdr/wasm.ts';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const ARTIFACT = resolve(root, 'public', 'dsp.wasm');
const FIXTURE = resolve(root, '..', 'tests', 'fixtures', 'drm', 'drm_live_modeB_so3_48828.f32');
/// Interleaved f32 I/Q of the committed live bench capture.
const COMPLEX = Math.floor(readFileSync(FIXTURE).length / 8);

const artifactBytes = () => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
};

const dsp = await instantiateDsp(artifactBytes());
const mode = new TextEncoder().encode('drm');
const modePtr = dsp.alloc(mode.length);
dsp.u8View(modePtr, mode.length).set(mode);
// The live capture was taken at the DDC's measured rate; the pipeline resamples to
// the DRM core rate itself.
const handle = dsp.exports.websa_dsp_demod_new(48828, 48000, modePtr, mode.length, 10000, 0);
dsp.free(modePtr, mode.length);
if (handle <= 0) throw new Error('demod_new failed');

const iq = new Float32Array(readFileSync(FIXTURE).buffer.slice(0, COMPLEX * 8));
const whole = process.argv.includes('--whole');
const BLOCK = 3248;
let pushed = 0;
if (whole) {
	const ptr = dsp.alloc(COMPLEX * 2 * 4);
	dsp.f32View(ptr, COMPLEX * 2).set(iq);
	pushed = dsp.exports.websa_dsp_demod_push(handle, ptr, COMPLEX);
	dsp.free(ptr, COMPLEX * 2 * 4);
} else {
	const blockPtr = dsp.alloc(BLOCK * 2 * 4);
	for (let off = 0; off < COMPLEX; off += BLOCK) {
		const n = Math.min(BLOCK, COMPLEX - off);
		dsp.f32View(blockPtr, n * 2).set(iq.subarray(off * 2, off * 2 + n * 2));
		pushed = dsp.exports.websa_dsp_demod_push(handle, blockPtr, n);
	}
	dsp.free(blockPtr, BLOCK * 2 * 4);
}

const rate = dsp.exports.websa_dsp_drm_audio_rate(handle);
const pcmCap = 48000 * 20;
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
console.log(
	JSON.stringify({ pushedLines: pushed, rate, samples, peak, meanAbs: samples ? +(energy / samples).toFixed(1) : 0 }, null, 2),
);
dsp.exports.websa_dsp_demod_free(handle);
