/**
 * Frame decoder contract test.
 *
 * Reads the golden fixtures produced by the Python encoders
 * (`tools/gen_frame_fixtures.py` → `tests/fixtures/frames/*.bin`) and asserts that
 * `core/frames.ts` decodes every field exactly. The Python side
 * (`tests/test_frame_fixtures.py`) asserts the fixtures still match its encoders, so the
 * two languages cannot drift apart without one of the two tests failing.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { decodeFrame, frameMagic, MAGIC_AUDIO, MAGIC_FREQ, MAGIC_POWR, MAGIC_RTAF } from '../core/frames';

// vitest runs with the frontend package as cwd; the fixtures live at the repository root.
const DIR = resolve(process.cwd(), '..', '..', 'tests', 'fixtures', 'frames') + '/';

const manifest = JSON.parse(readFileSync(`${DIR}manifest.json`, 'utf8')) as {
	freq: { version: number; points: number; sweep_ms: number; freq: number[] };
	powr: { version: number; points: number; sweep_ms: number; power: number[] };
	rta: {
		version: number; points: number; wf_len: number; max_density: number;
		start_hz: number; stop_hz: number; freq: number[]; spec: number[]; wf_row: number[];
	};
	audio: { seq: number; rate: number; pcm: number[] };
};

/** Copy the bytes so the fixture buffer is never mutated by a view. */
const bytes = (name: string): ArrayBuffer => {
	const buf = readFileSync(`${DIR}${name}`);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('decodeFrame', () => {
	it('decodes FREQ (float64 axis)', () => {
		const f = decodeFrame(bytes('freq.bin'));
		expect(f?.kind).toBe('freq');
		if (f?.kind !== 'freq') return;
		expect(f.version).toBe(manifest.freq.version);
		expect(f.points).toBe(manifest.freq.points);
		expect(f.sweepMs).toBeCloseTo(manifest.freq.sweep_ms, 6);
		expect(Array.from(f.freq)).toEqual(manifest.freq.freq);
	});

	it('decodes POWR (float32)', () => {
		const f = decodeFrame(bytes('powr.bin'));
		expect(f?.kind).toBe('powr');
		if (f?.kind !== 'powr') return;
		expect(f.version).toBe(manifest.powr.version);
		expect(f.points).toBe(manifest.powr.points);
		expect(Array.from(f.power).map((v) => Math.fround(v)))
			.toEqual(manifest.powr.power.map((v) => Math.fround(v)));
	});

	it('decodes RTAF (freq + spec + waterfall row + stop)', () => {
		const f = decodeFrame(bytes('rta.bin'));
		expect(f?.kind).toBe('rta');
		if (f?.kind !== 'rta') return;
		expect(f.version).toBe(manifest.rta.version);
		expect(f.points).toBe(manifest.rta.points);
		expect(f.wfLen).toBe(manifest.rta.wf_len);
		expect(f.maxDensity).toBe(manifest.rta.max_density);
		expect(f.startHz).toBe(manifest.rta.start_hz);
		expect(f.stopHz).toBe(manifest.rta.stop_hz);
		expect(Array.from(f.freq)).toEqual(manifest.rta.freq);
		expect(Array.from(f.spec).map((v) => Math.fround(v)))
			.toEqual(manifest.rta.spec.map((v) => Math.fround(v)));
		expect(Array.from(f.wfRow)).toEqual(manifest.rta.wf_row);
	});

	it('decodes AUDF (mono int16 PCM)', () => {
		const f = decodeFrame(bytes('audio.bin'));
		expect(f?.kind).toBe('audio');
		if (f?.kind !== 'audio') return;
		expect(f.seq).toBe(manifest.audio.seq);
		expect(f.rate).toBe(manifest.audio.rate);
		expect(f.samples).toBe(manifest.audio.pcm.length);
		expect(Array.from(f.pcm)).toEqual(manifest.audio.pcm);
	});

	it('reports the magic of each frame', () => {
		expect(frameMagic(bytes('freq.bin'))).toBe(MAGIC_FREQ);
		expect(frameMagic(bytes('powr.bin'))).toBe(MAGIC_POWR);
		expect(frameMagic(bytes('rta.bin'))).toBe(MAGIC_RTAF);
		expect(frameMagic(bytes('audio.bin'))).toBe(MAGIC_AUDIO);
	});

	it('rejects truncated and mis-sized frames instead of guessing', () => {
		const freq = new Uint8Array(bytes('freq.bin'));
		expect(decodeFrame(freq.slice(0, freq.length - 1).buffer)).toBeNull();   // one byte short
		expect(decodeFrame(freq.slice(0, 8).buffer)).toBeNull();                 // header only

		const rta = new Uint8Array(bytes('rta.bin'));
		expect(decodeFrame(rta.slice(0, 20).buffer)).toBeNull();                 // < RTA header
		expect(decodeFrame(rta.slice(0, rta.length - 4).buffer)).toBeNull();     // missing stopHz

		const audio = new Uint8Array(bytes('audio.bin'));
		expect(decodeFrame(audio.slice(0, audio.length - 2).buffer)).toBeNull(); // odd sample tail
	});

	it('ignores unknown magics and empty buffers', () => {
		expect(decodeFrame(new ArrayBuffer(0))).toBeNull();
		expect(decodeFrame(new Uint8Array([1, 2, 3]).buffer)).toBeNull();
		const unknown = new Uint8Array(32);
		unknown.set([0x41, 0x42, 0x43, 0x44], 0);   // 'ABCD'
		expect(decodeFrame(unknown.buffer)).toBeNull();
	});
});
