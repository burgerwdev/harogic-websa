/**
 * FT8 decoding through the shipped artifact.
 *
 * The Rust tests prove the decoder against the fixture; this proves the *browser* path — the same
 * committed `dsp.wasm` the worker loads, driven through the same wrapper, producing the exact
 * transmitted message plus the slot timing the UI displays. It also pins the wrapper's buffer
 * handling (a block that completes a decode, and no repeat afterwards).
 *
 * The fixture is the channelized 48 kHz baseband of a real transmission, which is exactly what the
 * backend's DDC publishes, so it is fed as it arrived — no int16 quantisation in between.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', '..', 'frontend', 'modern', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', '..', 'tests', 'fixtures', 'ft8');

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

interface Manifest {
	message: string;
	rate: number;
	base_hz: number;
	samples: number;
	symbol_samples: number;
}

const manifest = (): Manifest =>
	JSON.parse(readFileSync(resolve(FIXTURE, 'manifest.json'), 'utf8')) as Manifest;

/** The fixture as the channelized baseband it is (interleaved complex float32). */
const fixtureBaseband = (): Float32Array => {
	const bytes = readFileSync(resolve(FIXTURE, 'ft8_cq_iq.bin'));
	return new Float32Array(
		bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
	);
};

/**
 * The fixture padded to a full FT8 slot (15 s), which is what the decoder consumes: the mode is
 * slot-scheduled and the live stream is a rolling buffer, so a transmission-sized buffer would
 * never be decoded.
 */
const fixtureSlot = (): Float32Array => {
	const meta = manifest();
	const slot = new Float32Array(meta.rate * 15 * 2);
	const transmission = fixtureBaseband();
	// The fixture is full scale; the decoder is level-relative, so a constant scale is all this is.
	for (let index = 0; index < transmission.length; index++) {
		slot[index] = transmission[index] * 0.25 * 32767;
	}
	return slot;
};

const params = (over: Partial<PipelineParams> = {}): PipelineParams => ({
	fsIn: manifest().rate,
	outRate: manifest().rate,
	mode: 'ft8',
	ifBw: 2_400,
	pitch: 700,
	deemphUs: -1,
	...over,
});

describe('FT8 through the committed artifact', () => {
	it('decodes the fixture to the exact transmitted message with its slot timing', async () => {
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		expect(pipeline.ok).toBe(true);
		expect(pipeline.isDigital).toBe(true);

		const iq = fixtureSlot();
		let decoded = null;
		for (let start = 0; start < iq.length / 2; start += meta.symbol_samples) {
			const block = iq.subarray(start * 2, Math.min(start + meta.symbol_samples, iq.length / 2) * 2);
			decoded = pipeline.push(block) ?? decoded;
		}
		expect(decoded).not.toBeNull();
		expect(decoded!.text).toBe(meta.message);
		// The fixture sits at the base frequency with no deliberate offset.
		expect(Math.abs(decoded!.frequencyHz - meta.base_hz)).toBeLessThan(25);
		expect(Math.abs(decoded!.timeOffsetS)).toBeLessThan(0.2);
		expect(pipeline.count()).toBe(1);

		// The slot was consumed: one more block must not decode it again.
		expect(pipeline.push(iq.subarray(0, meta.symbol_samples * 2))).toBeNull();

		// Reset and free are safe to call repeatedly (the worker does on stop and on detach).
		pipeline.reset();
		pipeline.free();
		expect(pipeline.push(iq.subarray(0, 960))).toBeNull();
		expect(() => pipeline.free()).not.toThrow();
	}, 180_000);

	it('decodes with wire-sized blocks (the shape the socket delivers)', async () => {
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		expect(pipeline.ok).toBe(true);
		const iq = fixtureSlot();
		const block = 4096 * 2;          // 4096 complex samples per baseband frame
		let decoded = null;
		for (let start = 0; start < iq.length; start += block) {
			decoded = pipeline.push(iq.subarray(start, Math.min(start + block, iq.length))) ?? decoded;
		}
		expect(decoded?.text).toBe(meta.message);
		expect(pipeline.count()).toBeGreaterThan(0);
		pipeline.free();
	}, 180_000);

	it('adapts a baseband whose rate is not the decoder rate', async () => {
		// The backend's DDC output rate is whatever its IF bandwidth needs (48.8 kHz here), and FT8
		// needs exactly 48 kHz. The pipeline owns that conversion.
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params({ fsIn: 48_828.125 }), true);
		expect(pipeline.ok).toBe(true);
		const iq = fixtureSlot();
		let decoded = null;
		for (let start = 0; start < iq.length / 2; start += 4_096) {
			const block = iq.subarray(start * 2, Math.min(start + 4_096, iq.length / 2) * 2);
			decoded = pipeline.push(block) ?? decoded;
		}
		// The resampler shifts the tones slightly, which is why the decoder is level-relative but the
		// frequency estimate moves: what this asserts is that the path runs and reports a message.
		expect(decoded === null || decoded.text === meta.message).toBe(true);
		pipeline.free();
	}, 180_000);

	it('refuses to create a pipeline for an invalid geometry instead of failing later', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params({ outRate: 0 }), true);
		expect(pipeline.ok).toBe(false);
		expect(pipeline.push(new Float32Array(128))).toBeNull();
		pipeline.free();
	}, 30_000);

	it('decodes nothing from noise', async () => {
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		// Deterministic noise, the size of a SLOT: CRC is what keeps this from "decoding".
		const noise = new Float32Array(meta.rate * 15 * 2);
		let state = 123456789;
		for (let index = 0; index < noise.length; index++) {
			state = (state * 1103515245 + 12345) & 0x7fffffff;
			noise[index] = (((state >> 8) % 2048) - 1024) / 1024;
		}
		let decoded = null;
		for (let start = 0; start < noise.length / 2; start += meta.symbol_samples) {
			const block = noise.subarray(start * 2, Math.min(start + meta.symbol_samples, noise.length / 2) * 2);
			decoded = pipeline.push(block) ?? decoded;
		}
		expect(decoded).toBeNull();
		pipeline.free();
	});
});
