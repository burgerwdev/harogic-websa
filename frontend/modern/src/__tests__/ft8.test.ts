/**
 * FT8 decoding through the shipped artifact.
 *
 * The Rust tests prove the decoder against the fixture; this proves the *browser* path — the same
 * committed `dsp.wasm` the worker loads, driven through the same wrapper, producing the exact
 * transmitted message plus the slot timing the UI displays. It also pins the wrapper's buffer
 * handling (a block that completes a decode, and no repeat afterwards).
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { Ft8Session } from '../sdr/ft8';
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

const fixtureIq = (): Int16Array => {
	const meta = manifest();
	const bytes = readFileSync(resolve(FIXTURE, 'ft8_cq_iq.bin'));
	const complex = bytes.byteLength / 8;                 // f32 I/Q pairs
	const iq = new Int16Array(complex * 2);
	for (let index = 0; index < complex * 2; index++) {
		const value = bytes.readFloatLE(index * 4) * 0.25;  // the signal is full scale in the fixture
		iq[index] = Math.max(-32768, Math.min(32767, Math.round(value * 32767)));
	}
	expect(meta.samples).toBe(complex);
	return iq;
};

describe('FT8 through the committed artifact', () => {
	it('decodes the fixture to the exact transmitted message with its slot timing', async () => {
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const session = new Ft8Session(module, meta.rate);
		expect(session.ok).toBe(true);

		const iq = fixtureIq();
		let decoded = null;
		for (let start = 0; start < iq.length; start += meta.symbol_samples * 2) {
			const block = iq.subarray(start, Math.min(start + meta.symbol_samples * 2, iq.length));
			decoded = session.push(block) ?? decoded;
		}
		expect(decoded).not.toBeNull();
		expect(decoded!.text).toBe(meta.message);
		// The fixture sits at the base frequency with no deliberate offset.
		expect(Math.abs(decoded!.frequencyHz - meta.base_hz)).toBeLessThan(25);
		expect(Math.abs(decoded!.timeOffsetS)).toBeLessThan(0.2);
		expect(session.count()).toBe(1);

		// The transmission was consumed: one more block must not decode it again.
		expect(session.push(iq.subarray(0, meta.symbol_samples * 2))).toBeNull();

		// Reset and free are safe to call repeatedly (the worker does on stop and on detach).
		session.reset();
		session.free();
		expect(session.push(iq.subarray(0, 960))).toBeNull();
		expect(() => session.free()).not.toThrow();
	}, 180_000);

	it('refuses to create a session for an invalid rate instead of failing later', async () => {
		const module = await instantiateDsp(artifactBytes());
		const session = new Ft8Session(module, 0);
		expect(session.ok).toBe(false);
		expect(session.push(new Int16Array(64))).toBeNull();
		session.free();
	}, 30_000);

	it('decodes nothing from noise', async () => {
		const meta = manifest();
		const module = await instantiateDsp(artifactBytes());
		const session = new Ft8Session(module, meta.rate);
		// Deterministic noise, the size of a transmission: CRC is what keeps this from "decoding".
		const noise = new Int16Array(meta.symbol_samples * 2);
		let state = 123456789;
		for (let index = 0; index < noise.length; index++) {
			state = (state * 1103515245 + 12345) & 0x7fffffff;
			noise[index] = ((state >> 8) % 2048) - 1024;
		}
		let decoded = null;
		for (let start = 0; start + meta.symbol_samples * 2 <= noise.length; start += meta.symbol_samples * 2) {
			decoded = session.push(noise.subarray(start, start + meta.symbol_samples * 2)) ?? decoded;
		}
		expect(decoded).toBeNull();
		session.free();
	});
});
