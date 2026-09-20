/**
 * The WASM pipeline delivery contract, exercised against the committed artifact.
 *
 * This is the path the SDR worker runs per IQ block: allocate, process, read PCM back. Testing it
 * against the real `dsp.wasm` (not a mock) is what makes the ABI, the buffer sizes and the release
 * semantics trustworthy without a browser: a mismatch in any of them shows up here instead of as
 * silence on the bench.
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
	fsIn: 480_000,
	offsetHz: 0,
	decimate: 5,
	outRate: 48_000,
	mode: 'am',
	ifBw: 12_000,
	pitch: 700,
};

/** Amplitude-modulated channel at DC, 1 kHz modulation: the DDC's output for a real AM signal. */
function amIq(samples: number): Int16Array {
	const iq = new Int16Array(samples * 2);
	for (let k = 0; k < samples; k++) {
		const envelope = 1.0 + 0.5 * Math.cos((2 * Math.PI * 1000 * k) / PARAMS.outRate);
		iq[2 * k] = Math.round(envelope * 0.4 * 32768);
		iq[2 * k + 1] = 0;
	}
	return iq;
}

const rms = (pcm: Float32Array): number => {
	let sum = 0;
	for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
	return pcm.length ? Math.sqrt(sum / pcm.length) : 0;
};

describe('the WASM receive pipeline behind the SDR worker', () => {
	it('turns an IQ block into audible PCM', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS);
		expect(pipeline.ok).toBe(true);
		expect(pipeline.mode).toBe('am');

		let pcm = new Float32Array(0);
		for (let block = 0; block < 6; block++) {
			pcm = pipeline.process(amIq(4_800));
		}
		expect(pcm.length).toBeGreaterThan(0);
		// The transport claim is "IQ in, PCM out". The level is the chain's policy (the adaptive
		// notch removes the strongest tone, the squelch gates the remainder), so a modest floor is
		// asserted here and the per-mode level is verified on the bench (task 12).
		expect(rms(pcm)).toBeGreaterThan(0.002);
		pipeline.free();
	});

	it('applies volume after the DSP, not inside it', async () => {
		const module = await instantiateDsp(artifactBytes());
		const quiet = new WasmPipeline(module, PARAMS);
		quiet.setVolume(0.25);
		let a = new Float32Array(0);
		for (let block = 0; block < 6; block++) a = quiet.process(amIq(4_800));
		quiet.free();

		const loud = new WasmPipeline(module, PARAMS);
		let b = new Float32Array(0);
		for (let block = 0; block < 6; block++) b = loud.process(amIq(4_800));
		loud.free();

		expect(a.length).toBe(b.length);
		expect(rms(a)).toBeLessThan(rms(b) * 0.5);
	});

	it('refuses a mode the registry does not implement instead of running silently', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, { ...PARAMS, mode: 'not_a_mode' });
		expect(pipeline.ok).toBe(false);
		expect(pipeline.process(amIq(960)).length).toBe(0);
		pipeline.free();
	});

	it('survives reconfigure and free without leaking the handle', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS);
		for (let block = 0; block < 3; block++) pipeline.process(amIq(4_800));
		pipeline.reconfigure({ ...PARAMS, mode: 'nfm', ifBw: 12_000 });
		expect(pipeline.ok).toBe(true);
		expect(pipeline.mode).toBe('nfm');
		let pcm = new Float32Array(0);
		for (let block = 0; block < 3; block++) pcm = pipeline.process(amIq(4_800));
		// The pipeline keeps producing blocks of PCM after a reconfigure; the *content* depends on
		// the new mode's detector, so the length is what this contract asserts.
		expect(pcm.length).toBeGreaterThan(0);
		pipeline.free();
		expect(pipeline.process(amIq(960)).length).toBe(0);
		// Freeing twice must not throw: the worker frees on stop and on detach.
		expect(() => pipeline.free()).not.toThrow();
	});

	it('switches the audio-enhancement chain without breaking the output', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, PARAMS);
		pipeline.setAudioEnabled(false);
		let withoutChain = new Float32Array(0);
		for (let block = 0; block < 6; block++) withoutChain = pipeline.process(amIq(4_800));
		pipeline.setAudioEnabled(true);
		let withChain = new Float32Array(0);
		for (let block = 0; block < 6; block++) withChain = pipeline.process(amIq(4_800));
		pipeline.free();
		expect(withoutChain.length).toBeGreaterThan(0);
		expect(withChain.length).toBeGreaterThan(0);
		pipeline.free();
	});
});
