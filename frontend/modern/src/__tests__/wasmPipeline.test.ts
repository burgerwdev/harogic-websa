/**
 * The WASM demodulator delivery contract, exercised against the committed artifact.
 *
 * This is the path the SDR worker runs per baseband block: allocate, process, read the result back.
 * Testing it against the real `dsp.wasm` (not a mock) is what makes the ABI, the buffer sizes and
 * the release semantics trustworthy without a browser: a mismatch in any of them shows up here
 * instead of as silence on the bench.
 *
 * The input is the backend's channelized baseband (a carrier at DC with modulation on it), which is
 * what the DDC hands over — not raw IQ at the analyzer's rate.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const ARTIFACT = resolve(process.cwd(), '..', '..', 'frontend', 'modern', 'public', 'dsp.wasm');

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

const PARAMS: PipelineParams = {
	fsIn: 48_000,
	outRate: 48_000,
	mode: 'am',
	ifBw: 12_000,
	pitch: 700,
	deemphUs: -1,             // the mode's default (Auto)
};

/** The DDC's output for a real AM signal: a carrier at DC, modulated at 1 kHz. */
function amBaseband(samples: number): Float32Array {
	const iq = new Float32Array(samples * 2);
	for (let k = 0; k < samples; k++) {
		const envelope = 1.0 + 0.5 * Math.cos((2 * Math.PI * 1000 * k) / PARAMS.fsIn);
		iq[2 * k] = envelope * 0.25;
		iq[2 * k + 1] = 0;
	}
	return iq;
}

const rms = (pcm: Float32Array): number => {
	let sum = 0;
	for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
	return pcm.length ? Math.sqrt(sum / pcm.length) : 0;
};

describe('the WASM demodulator behind the SDR worker', () => {
	it('turns a baseband block into audible PCM', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS, false);
		expect(pipeline.ok).toBe(true);
		expect(pipeline.mode).toBe('am');

		let pcm = new Float32Array(0);
		for (let block = 0; block < 6; block++) {
			pcm = pipeline.process(amBaseband(4_800));
		}
		expect(pcm.length).toBeGreaterThan(0);
		// The transport claim is "baseband in, PCM out". The level is the chain's policy (the
		// adaptive notch removes the strongest tone, the squelch gates the remainder), so a modest
		// floor is asserted here.
		expect(rms(pcm)).toBeGreaterThan(0.002);
		pipeline.free();
	});

	it('produces the rate the consumer asked for, not a constant', async () => {
		// One second of baseband at the DDC's rate must come back as one second at the AudioWorklet's
		// rate: a 44.1 kHz device used to be handed 48 kHz and played 8.8% slow.
		const module = await instantiateDsp(artifactBytes());
		const baseband = amBaseband(48_000);
		for (const outRate of [48_000, 44_100]) {
			const pipeline = new WasmPipeline(module, { ...PARAMS, outRate }, false);
			let total = 0;
			for (let start = 0; start + 4_800 <= baseband.length / 2; start += 4_800) {
				total += pipeline.process(baseband.subarray(start * 2, (start + 4_800) * 2)).length;
			}
			pipeline.free();
			expect(Math.abs(total - outRate)).toBeLessThan(outRate * 0.01);
		}
	});

	it('applies volume after the DSP, not inside it', async () => {
		const module = await instantiateDsp(artifactBytes());
		const quiet = new WasmPipeline(module, PARAMS, false);
		quiet.setVolume(0.25);
		let a = new Float32Array(0);
		for (let block = 0; block < 6; block++) a = quiet.process(amBaseband(4_800));
		quiet.free();

		const loud = new WasmPipeline(module, PARAMS, false);
		let b = new Float32Array(0);
		for (let block = 0; block < 6; block++) b = loud.process(amBaseband(4_800));
		loud.free();

		expect(a.length).toBe(b.length);
		expect(rms(a)).toBeLessThan(rms(b) * 0.5);
	});

	it('refuses a mode the registry does not implement instead of running silently', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, { ...PARAMS, mode: 'not_a_mode' }, false);
		expect(pipeline.ok).toBe(false);
		expect(pipeline.process(amBaseband(960)).length).toBe(0);
		pipeline.free();
	});

	it('survives reconfigure and free without leaking the handle', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS, false);
		for (let block = 0; block < 3; block++) pipeline.process(amBaseband(4_800));
		pipeline.reconfigure({ ...PARAMS, mode: 'nfm', ifBw: 12_000 }, false);
		expect(pipeline.ok).toBe(true);
		expect(pipeline.mode).toBe('nfm');
		let pcm = new Float32Array(0);
		for (let block = 0; block < 3; block++) pcm = pipeline.process(amBaseband(4_800));
		// The pipeline keeps producing blocks of PCM after a reconfigure; the *content* depends on
		// the new mode's detector, so the length is what this contract asserts.
		expect(pcm.length).toBeGreaterThan(0);
		pipeline.free();
		expect(pipeline.process(amBaseband(960)).length).toBe(0);
		// Freeing twice must not throw: the worker frees on stop and on detach.
		expect(() => pipeline.free()).not.toThrow();
	});

	it('switches the audio-enhancement chain without breaking the output', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS, false);
		pipeline.setAudioEnabled(false);
		let withoutChain = new Float32Array(0);
		for (let block = 0; block < 6; block++) withoutChain = pipeline.process(amBaseband(4_800));
		pipeline.setAudioEnabled(true);
		let withChain = new Float32Array(0);
		for (let block = 0; block < 6; block++) withChain = pipeline.process(amBaseband(4_800));
		pipeline.free();
		expect(withoutChain.length).toBeGreaterThan(0);
		expect(withChain.length).toBeGreaterThan(0);
		pipeline.free();
	});
});
