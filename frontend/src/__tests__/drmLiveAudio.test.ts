/**
 * DRM live-capture audio regression: the committed bench capture (a real HE-AAC
 * stream: 12 kHz AAC core with SBR, mono) must decode to non-silent 24 kHz PCM
 * through the full receive chain, like Dream does on the same samples. This is the
 * wasm32 gate for the audio path — the native suites only see the access units,
 * because the FDK decoder is linked into the wasm module alone.
 *
 * The capture fed `au=0` and then trapped inside FDK's HCR decoder until the aligned
 * allocator in wasm/fdk/shim.cpp honoured FDK's "malloc and clear" semantics; this
 * test fails if either the AU stream or the allocator regress.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', 'tests', 'fixtures', 'drm', 'drm_live_modeB_so3_48828.f32');
/// 6 s of interleaved f32 I/Q at ~48.8 kHz, fed in worker-sized blocks.
const COMPLEX = Math.floor(readFileSync(FIXTURE).length / 8);
const BLOCK = 3248;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('DRM live-capture audio (HE-AAC 12 kHz core + SBR)', () => {
	for (const outputRate of [48_000, 44_100]) it(`decodes at a ${outputRate} Hz sound-card rate`, async () => {
		const dsp = await instantiateDsp(artifactBytes());

		const mode = new TextEncoder().encode('drm');
		const modePtr = dsp.alloc(mode.length);
		dsp.u8View(modePtr, mode.length).set(mode);
		// The capture is at the channelizer's measured rate; the pipeline resamples.
		const handle = dsp.exports.websa_dsp_demod_new(48828, outputRate, modePtr, mode.length, 10000, 0);
		dsp.free(modePtr, mode.length);
		expect(handle).toBeGreaterThan(0);

		const iq = new Float32Array(
			readFileSync(FIXTURE).buffer.slice(0, COMPLEX * 8),
		);
		const blockPtr = dsp.alloc(BLOCK * 2 * 4);
		for (let off = 0; off < COMPLEX; off += BLOCK) {
			const n = Math.min(BLOCK, COMPLEX - off);
			dsp.f32View(blockPtr, n * 2).set(iq.subarray(off * 2, off * 2 + n * 2));
			dsp.exports.websa_dsp_demod_push(handle, blockPtr, n);
		}
		dsp.free(blockPtr, BLOCK * 2 * 4);

		// SBR doubles the 12 kHz core rate, as Dream reports it.
		const rate = dsp.exports.websa_dsp_drm_audio_rate(handle);
		expect(rate).toBe(24000);

		const pcmPtr = dsp.alloc(48000 * 20 * 2);
		const samples = dsp.exports.websa_dsp_drm_audio_pcm(handle, pcmPtr, 48000 * 20);
		// Three-plus decode passes of a 6 s capture; the long interleaver needs ~2 s
		// before the first access units exist.
		expect(samples).toBeGreaterThan(24000);

		const pcm = dsp.i16View(pcmPtr, samples);
		let peak = 0;
		let energy = 0;
		for (let i = 0; i < samples; i++) {
			const v = Math.abs(pcm[i]);
			peak = Math.max(peak, v);
			energy += v;
		}
		expect(peak).toBeGreaterThan(1000);
		expect(energy / samples).toBeGreaterThan(100);

		// The capture carries the bench transmitter's 1 kHz tone. A regression to noise —
		// the DRM text message left inside the last access unit of every super frame once
		// shifted FDK's backwards SBR read, which produced exactly that — shows up here as
		// a missing tone instead of the peak below.
		const n = Math.min(rate, samples);
		const goertzel = (hz: number): number => {
			const w = (2 * Math.PI * hz) / rate;
			const c = 2 * Math.cos(w);
			let s1 = 0;
			let s2 = 0;
			for (let i = 0; i < n; i++) {
				const s0 = pcm[i] + c * s1 - s2;
				s2 = s1;
				s1 = s0;
			}
			return Math.sqrt(Math.abs(s1 * s1 + s2 * s2 - c * s1 * s2));
		};
		let bestHz = 0;
		let bestMag = 0;
		let sumMag = 0;
		let count = 0;
		for (let hz = 400; hz <= 2000; hz += 20) {
			const mag = goertzel(hz);
			if (mag > bestMag) {
				bestMag = mag;
				bestHz = hz;
			}
			sumMag += mag;
			count++;
		}
		expect(Math.abs(bestHz - 1000)).toBeLessThanOrEqual(60);
		expect(bestMag).toBeGreaterThan((16 * sumMag) / count);

		// Draining read: the worker relies on it to forward each sample exactly once, and a
		// head-window read instead of a drain was what silenced every session after ~1.4 s
		// (the accumulated buffer outgrew the read cap and the worker's offset went stale).
		expect(dsp.exports.websa_dsp_drm_audio_pcm(handle, pcmPtr, 48000 * 20)).toBe(0);

		dsp.free(pcmPtr, 48000 * 20 * 2);
		dsp.exports.websa_dsp_demod_free(handle);
	});
});
