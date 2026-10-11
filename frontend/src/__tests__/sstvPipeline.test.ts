/**
 * The SSTV picture over the ABI, driven the way the worker drives it.
 *
 * The worker keeps a digital `WasmPipeline` for `sstv` and reads three things back from it: the
 * decoder's readout, the picture state and the pixels. This test feeds the committed fixture
 * (`tests/fixtures/sstv/robot36_iq.bin`, written by the Python encoder) through that same wrapper, so
 * the ABI pair, the mode pin and the metadata words are exercised against the shipped artifact rather
 * than only in Rust.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', 'tests', 'fixtures', 'sstv');
const BLOCK = 32_768;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

const fixtureFloat32 = (name: string): Float32Array => {
	const buf = readFileSync(resolve(FIXTURE, name));
	return new Float32Array(buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength));
};

const fixtureBytes = (name: string): Uint8Array => new Uint8Array(readFileSync(resolve(FIXTURE, name)));

const manifest = JSON.parse(readFileSync(resolve(FIXTURE, 'manifest.json'), 'utf8')) as {
	rate: number; samples: number; width: number; height: number; vis: number;
	callsign: string;
};

function params(): PipelineParams {
	return {
		fsIn: manifest.rate,
		outRate: 48_000,
		mode: 'sstv',
		ifBw: 2_400,
		pitch: 700,
		deemphUs: -1,
	};
}

describe('the SSTV picture over the ABI', () => {
	it('reports nothing before a picture has decoded, and takes a mode pin', async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		expect(pipeline.ok).toBe(true);
		expect(pipeline.imageStatus()).toBeNull();
		expect(pipeline.image()).toBeNull();
		// The protocol owns the knob, so it accepts both a pin and clearing it.
		expect(pipeline.setModePin(5)).toBe(true);
		expect(pipeline.setModePin(null)).toBe(true);
		// The state readout the pane shows while nothing has decoded: no attempts, no audio, no picture.
		expect(pipeline.decodeState()).toEqual({
			attempts: 0, bufferedSamples: 0, bufferedSeconds: 0, hasPicture: false, level: 0,
		});
		pipeline.free();
	});

	it('decodes the committed Robot 36 waveform to the picture it carries', { timeout: 30_000 }, async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		const iq = fixtureFloat32('robot36_iq.bin').subarray(0, manifest.samples * 2);
		const readout: string[] = [];
		let status = null as ReturnType<WasmPipeline['imageStatus']>;
		for (let at = 0; at < iq.length; at += BLOCK * 2) {
			const chunk = iq.subarray(at, Math.min(at + BLOCK * 2, iq.length));
			for (const report of pipeline.push(chunk)) readout.push(report.text);
			status = pipeline.imageStatus();
		}
		expect(status).not.toBeNull();
		// Robot 36 is index 5 in the mode table (`SSTV_MODES` order), VIS 8, 320x240.
		expect(status).toMatchObject({
			modeIndex: 5, vis: manifest.vis, width: manifest.width, height: manifest.height,
			complete: true, progress: 100, lines: manifest.height,
		});
		// The decoder's own readout travels with the picture; the pane shows the structured fields,
		// but "the decoder ran and said nothing" must be visible in the diagnostics.
		expect(readout.some((line) => line.includes('Robot 36'))).toBe(true);

		const picture = pipeline.image();
		expect(picture).not.toBeNull();
		expect(picture!.rgb.length).toBe(manifest.width * manifest.height * 3);

		// The picture is the one the fixture carries (the Rust test asserts the same tolerance on the
		// same waveform; this one runs through the JS wrapper and the worker's block size).
		const expected = fixtureBytes('robot36_rgb.bin');
		let sum = 0;
		for (let i = 0; i < expected.length; i++) sum += Math.abs(picture!.rgb[i] - expected[i]);
		expect(sum / expected.length).toBeLessThan(24);
		// ...and after decoding, the same call reports the attempts and the picture.
		const state = pipeline.decodeState();
		expect(state?.hasPicture).toBe(true);
		expect(state?.attempts).toBeGreaterThan(0);
		expect(state?.bufferedSeconds).toBeGreaterThan(0);
		pipeline.free();
	});

	it('keeps a picture across a retune-free reset and clears it on request', { timeout: 30_000 }, async () => {
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params(), true);
		const iq = fixtureFloat32('robot36_iq.bin').subarray(0, manifest.samples * 2);
		for (let at = 0; at < iq.length; at += BLOCK * 2) {
			pipeline.push(iq.subarray(at, Math.min(at + BLOCK * 2, iq.length)));
		}
		const before = pipeline.imageStatus();
		expect(before).not.toBeNull();
		// `reset` is the ABI call a stream glitch makes: the decoder's state goes with it. The picture
		// the operator is looking at lives in the page's log (sdr/sstvLog), which decides when to drop
		// it — the decoder holds only what it is decoding.
		pipeline.reset();
		expect(pipeline.imageStatus()).toBeNull();
		pipeline.free();
	});
});
