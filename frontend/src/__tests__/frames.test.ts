/**
 * Frame decoder contract test.
 *
 * Reads the golden fixtures produced by the Python encoders
 * (`tools/fixtures/gen_frame_fixtures.py` → `tests/fixtures/frames/*.bin`) and asserts that
 * `core/frames.ts` decodes every field exactly. The Python side
 * (`tests/test_frame_fixtures.py`) asserts the fixtures still match its encoders, so the
 * two languages cannot drift apart without one of the two tests failing.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { BASEBAND_HEADER_BYTES, decodeFrame, frameMagic, MAGIC_AUXILIARY, MAGIC_BASEBAND, MAGIC_FREQ, MAGIC_POWR, MAGIC_RTAF } from '../core/frames';
import type { TriggerMetadata } from '../ui/triggerEvents';

// vitest runs with the frontend package as cwd; the fixtures live at the repository root.
const DIR = resolve(process.cwd(), '..', 'tests', 'fixtures', 'frames') + '/';

interface BasebandFixture {
	version: number; seq: number; samples: number; rate: number; center_hz: number; iq: number[];
	channel_id: number; epoch: number; input_index: string | number; delay_samples: number;
}
const manifest = JSON.parse(readFileSync(`${DIR}manifest.json`, 'utf8')) as {
	freq: { version: number; points: number; sweep_ms: number; freq: number[] };
	powr: { version: number; points: number; sweep_ms: number; power: number[] };
	rta: {
		version: number; points: number; wf_len: number; max_density: number;
		start_hz: number; stop_hz: number; freq: number[]; spec: number[]; wf_row: number[];
	};
	rta2: { metadata: TriggerMetadata };
	baseband: BasebandFixture;
	auxiliary: BasebandFixture;
};

/** Copy the bytes so the fixture buffer is never mutated by a view. */
const bytes = (name: string): ArrayBuffer => {
	const buf = readFileSync(`${DIR}${name}`);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};
const boundary = (name: 'baseband' | 'auxiliary'): ArrayBuffer => {
	const head = bytes(`${name}.bin`).slice(0, BASEBAND_HEADER_BYTES);
	new DataView(head).setUint32(8, 0, true);                 // seq 0
	new DataView(head).setUint32(12, 0, true);                // samples 0
	return head;
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

	it('decodes RTA2 capture metadata correlated with its spectrum', () => {
		const frame = decodeFrame(bytes('rta2.bin'));
		expect(frame?.kind).toBe('rta');
		if (frame?.kind !== 'rta') return;
		expect(frame.trigger).toEqual(manifest.rta2.metadata);
		expect(Array.from(frame.freq)).toEqual(manifest.rta.freq);
		expect(Array.from(frame.spec)).toEqual(manifest.rta.spec.map(Math.fround));
		expect(frame.stopHz).toBe(manifest.rta.stop_hz);
	});

	it('rejects truncated, malformed and overlong RTA2 capture trailers', () => {
		const data = bytes('rta2.bin');
		const base = bytes('rta.bin').byteLength;
		for (const length of [base - 1, base, base + 3, base + 7, data.byteLength - 1]) {
			expect(decodeFrame(data.slice(0, length))).toBeNull();
		}
		const malformed = data.slice(0);
		new DataView(malformed).setUint32(base, 0, true);
		expect(decodeFrame(malformed)).toBeNull();
		const extra = new Uint8Array(data.byteLength + 1);
		extra.set(new Uint8Array(data));
		expect(decodeFrame(extra.buffer)).toBeNull();
		const invalidJson = data.slice(0);
		new Uint8Array(invalidJson)[base + 8] = 0;
		expect(decodeFrame(invalidJson)).toBeNull();
	});

	it.each(['baseband', 'auxiliary'] as const)('decodes %s v2 with an aligned zero-copy payload', (name) => {
		const data = bytes(`${name}.bin`);
		const expected = manifest[name];
		const f = decodeFrame(data);
		expect(f?.kind).toBe('baseband');
		if (f?.kind !== 'baseband') return;
		expect(f.version).toBe(2);
		expect(f.magic).toBe(name === 'baseband' ? MAGIC_BASEBAND : MAGIC_AUXILIARY);
		expect(f.seq).toBe(expected.seq);
		expect(f.samples).toBe(expected.samples);
		expect(f.rate).toBeCloseTo(expected.rate, 6);
		expect(f.centerHz).toBeCloseTo(expected.center_hz, 6);
		expect(f.channelId).toBe(expected.channel_id);
		expect(f.epoch).toBe(expected.epoch);
		expect(f.inputIndex).toBe(BigInt(expected.input_index));
		expect(f.groupDelay).toBe(expected.delay_samples);
		expect(f.iq.buffer).toBe(data);
		expect(f.iq.byteOffset).toBe(BASEBAND_HEADER_BYTES);
		expect(Array.from(f.iq)).toEqual(expected.iq.map(Math.fround));
	});

	it('reports the magic of each frame', () => {
		expect(frameMagic(bytes('freq.bin'))).toBe(MAGIC_FREQ);
		expect(frameMagic(bytes('powr.bin'))).toBe(MAGIC_POWR);
		expect(frameMagic(bytes('rta.bin'))).toBe(MAGIC_RTAF);
		expect(frameMagic(bytes('baseband.bin'))).toBe(MAGIC_BASEBAND);
		expect(frameMagic(bytes('auxiliary.bin'))).toBe(MAGIC_AUXILIARY);
	});

	it('rejects truncated and mis-sized frames instead of guessing', () => {
		const freq = new Uint8Array(bytes('freq.bin'));
		expect(decodeFrame(freq.slice(0, freq.length - 1).buffer)).toBeNull();   // one byte short
		expect(decodeFrame(freq.slice(0, 8).buffer)).toBeNull();                 // header only

		const rta = new Uint8Array(bytes('rta.bin'));
		expect(decodeFrame(rta.slice(0, 20).buffer)).toBeNull();                 // < RTA header
		expect(decodeFrame(rta.slice(0, rta.length - 4).buffer)).toBeNull();     // missing stopHz

		const baseband = new Uint8Array(bytes('baseband.bin'));
		expect(decodeFrame(baseband.slice(0, 16).buffer)).toBeNull();            // < baseband header
		expect(decodeFrame(baseband.slice(0, baseband.length - 4).buffer)).toBeNull();  // one I/Q short
	});

	it.each(['baseband', 'auxiliary'] as const)('rejects malformed %s v2 headers and nonempty boundaries', (name) => {
		for (const version of [0, 1, 3]) {
			const data = bytes(`${name}.bin`);
			new DataView(data).setUint32(4, version, true);
			expect(decodeFrame(data)).toBeNull();
		}
		const data = bytes(`${name}.bin`);
		expect(decodeFrame(data.slice(0, 55))).toBeNull();
		const extra = new Uint8Array(data.byteLength + 8);
		extra.set(new Uint8Array(data));
		expect(decodeFrame(extra.buffer)).toBeNull();            // trailing bytes
		new DataView(data).setUint32(8, 0, true);
		expect(decodeFrame(data)).toBeNull();                    // seq0 cannot carry IQ
		const emptyPayload = data.slice(0, BASEBAND_HEADER_BYTES);
		expect(decodeFrame(emptyPayload)).toBeNull();            // declared samples lack payload
		expect(decodeFrame(boundary(name))).toMatchObject({
			kind: 'baseband', seq: 0, samples: 0,
			channelId: manifest[name].channel_id, epoch: manifest[name].epoch,
		});
		const nonBoundary = boundary(name);
		new DataView(nonBoundary).setUint32(8, 1, true);
		expect(decodeFrame(nonBoundary)).toBeNull();              // only boundaries may be empty
	});

	it('preserves all u64 input clock bits including the unavailable sentinel', () => {
		for (const name of ['baseband', 'auxiliary'] as const) {
			for (const index of [9007199254740993n, 18446744073709551615n]) {
				const data = bytes(`${name}.bin`);
				new DataView(data).setBigUint64(40, index, true);
				const f = decodeFrame(data);
				expect(f?.kind === 'baseband' && f.inputIndex).toBe(index);
			}
		}
	});

	it('ignores unknown magics and empty buffers', () => {
		expect(decodeFrame(new ArrayBuffer(0))).toBeNull();
		expect(decodeFrame(new Uint8Array([1, 2, 3]).buffer)).toBeNull();
		const unknown = new Uint8Array(32);
		unknown.set([0x41, 0x42, 0x43, 0x44], 0);   // 'ABCD'
		expect(decodeFrame(unknown.buffer)).toBeNull();
	});
});
