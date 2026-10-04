/**
 * DRM live-capture audio check (wasm, node-local): feeds the committed live bench
 * capture (12 kHz HE-AAC core with SBR, 48828.125 Hz DDC rate) through the full
 * receive chain in 3248-sample blocks, like the worker does, and reports the PCM the
 * `websa_dsp_drm_audio_pcm` ABI surfaces. A diagnostic tool, not a gate — the
 * regression gate is frontend/src/__tests__/drmLiveAudio.test.ts.
 *
 *   node --experimental-strip-types scripts/drm_live_audio.mjs [--whole]
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { instantiateDsp } from '../src/sdr/wasm.ts';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const ARTIFACT = resolve(root, 'public', 'dsp.wasm');
const FIXTURE = resolve(root, '..', 'tests', 'fixtures', 'drm', 'drm_live_modeB_so3_48828.f32');
const argFile = process.argv.find((a) => a.endsWith('.f32'));
const FILE = argFile ? resolve(argFile) : FIXTURE;
/// Interleaved f32 I/Q of the capture.
const COMPLEX = Math.floor(readFileSync(FILE).length / 8);

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
const argRate = Number(process.argv.find((a) => /^\d{4,6}(\.\d+)?$/.test(a) && !a.includes('e')) ?? 0);
const IN_RATE = argRate || 48828;
const handle = dsp.exports.websa_dsp_demod_new(IN_RATE, 48000, modePtr, mode.length, 10000, 0);
dsp.free(modePtr, mode.length);
if (handle <= 0) throw new Error('demod_new failed');

const iq = new Float32Array(readFileSync(FILE).buffer.slice(0, COMPLEX * 8));
const whole = process.argv.includes('--whole');
const BLOCK = 3248;
let pushed = 0;
const textPtr = dsp.alloc(4096);
const metricsPtr = dsp.alloc(3 * 8);
let lastLines = [];
const readMessages = () => {
	const count = dsp.exports.websa_dsp_demod_messages
		? dsp.exports.websa_dsp_demod_messages(handle)
		: pushed;
	for (let m = 0; m < count; m++) {
		const len = dsp.exports.websa_dsp_demod_message_at(handle, m, textPtr, 4096, metricsPtr);
		if (len > 0) lastLines.push(new TextDecoder().decode(dsp.u8View(textPtr, len)));
	}
};
if (whole) {
	const ptr = dsp.alloc(COMPLEX * 2 * 4);
	dsp.f32View(ptr, COMPLEX * 2).set(iq);
	pushed = dsp.exports.websa_dsp_demod_push(handle, ptr, COMPLEX);
	dsp.free(ptr, COMPLEX * 2 * 4);
} else {
	const blockPtr = dsp.alloc(BLOCK * 2 * 4);
	const incPtr = dsp.alloc(65536 * 2);
	let incTotal = 0;
	let incBlocks = 0;
	const incremental = Boolean(process.env.WEBSA_DRM_INCREMENTAL);
	for (let off = 0; off < COMPLEX; off += BLOCK) {
		const n = Math.min(BLOCK, COMPLEX - off);
		dsp.f32View(blockPtr, n * 2).set(iq.subarray(off * 2, off * 2 + n * 2));
		pushed = dsp.exports.websa_dsp_demod_push(handle, blockPtr, n);
		if (incremental) {
			// Drain like the DSP worker does, so a delivery stall shows up here too.
			incTotal += dsp.exports.websa_dsp_drm_audio_pcm(handle, incPtr, 65536);
			incBlocks++;
			if (incBlocks % 20 === 0) {
				console.error(`  [inc] blocks=${incBlocks} pcm_total=${incTotal}`);
			}
		}
	}
	dsp.free(incPtr, 65536 * 2);
	if (incremental) console.error(`  [inc] final pcm_total=${incTotal}`);
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
// Dump the PCM too, so the decoded audio content (not just "non-silent") can be
// checked with a spectrum analysis.
const dumpPcm = process.env.WEBSA_DRM_PCM_DUMP;
if (dumpPcm && samples > 0) {
	writeFileSync(dumpPcm, Buffer.from(pcm.buffer, pcm.byteOffset, samples * 2));
}

console.log(
	JSON.stringify(
		{ pushedLines: pushed, rate, samples, peak, meanAbs: samples ? +(energy / samples).toFixed(1) : 0, readout: lastLines.slice(-8) },
		null,
		2,
	),
);
dsp.exports.websa_dsp_demod_free(handle);
