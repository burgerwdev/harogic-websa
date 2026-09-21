/** Scratch: decode a real 40 m capture through the shipped artifact (the phone app decoded it). */
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

describe('probe', () => {
	it('decodes the real 40 m capture', async () => {
		const meta = JSON.parse(readFileSync('/tmp/real_ft8.json', 'utf8'));
		const bytes = readFileSync('/tmp/real_ft8.iq');
		const iq = new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
		const module = await instantiateDsp(artifactBytes());
		for (const outRate of [Math.round(meta.rate), 48_000]) {
			const params: PipelineParams = {
				fsIn: meta.rate, outRate, mode: 'ft8', ifBw: 2_400, pitch: 700, deemphUs: -1,
			};
			const pipeline = new WasmPipeline(module, params, true);
			const block = 4_096;
			const decodes: string[] = [];
			for (let start = 0; start + block * 2 <= iq.length; start += block * 2) {
				const m = pipeline.push(iq.subarray(start, start + block * 2));
				if (m) decodes.push(`${m.text} @ ${m.frequencyHz.toFixed(0)} Hz snr ${m.snrDb.toFixed(0)}`);
			}
			// eslint-disable-next-line no-console
			console.log(`PROBE outRate=${outRate}: ${decodes.length} decodes ${JSON.stringify(decodes.slice(0, 6))}`);
			pipeline.free();
		}
		expect(iq.length).toBeGreaterThan(0);
	}, 600_000);
});
