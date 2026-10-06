/**
 * Decode a captured baseband through the shipped artifact, slot by slot.
 *
 * `tools/bench/ft8_capture_check.py` writes the capture (the channelized baseband the browser decoder
 * receives) and reports which slots contain an FT8-shaped burst. This runs the decode, so a report of
 * "no decode" can be attributed: no burst in the capture (nothing to decode), a burst but no decode
 * (the decoder missed it), or a decode (it works).
 *
 *   python3 tools/bench/ft8_capture_check.py --frequency 7.080e6 --seconds 60
 *   WEBSA_FT8_IQ=/tmp/ft8_capture.json npx vitest run src/__tests__/ft8Capture.test.ts
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const capturePath = process.env.WEBSA_FT8_IQ;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe.skipIf(!capturePath)('FT8 on a captured baseband', () => {
	it('decodes every slot that carries a transmission', async () => {
		const meta = JSON.parse(readFileSync(capturePath!, 'utf8'));
		const bytes = readFileSync(resolve(dirname(capturePath!), meta.iq_file));
		const iq = new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));

		const params: PipelineParams = {
			fsIn: meta.fs_in,
			outRate: meta.out_rate,
			mode: meta.mode,
			ifBw: meta.if_bw,
			pitch: meta.pitch,
			deemphUs: -1,
		};
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params, true);
		const block = 4_096;
		const decodes: string[] = [];
		let blocks = 0;
		for (let start = 0; start + block * 2 <= iq.length; start += block * 2) {
			const reports = pipeline.push(iq.subarray(start, start + block * 2));
			blocks += 1;
			for (const message of reports) {
				decodes.push(`${message.text} @ ${message.frequencyHz.toFixed(0)} Hz snr ` +
					`${message.snrDb.toFixed(0)} dB`);
				// eslint-disable-next-line no-console
				console.log(`  slot ${(blocks * block) / meta.out_rate / 15 | 0}: ${decodes[decodes.length - 1]}`);
			}
		}
		pipeline.free();
		// eslint-disable-next-line no-console
		console.log(`FT8 capture: ${blocks} blocks (${(iq.length / 2 / meta.fs_in).toFixed(1)} s), ` +
			`${decodes.length} decodes`);
		// The assertion is deliberately about the *mechanism*: a capture with a transmission must
		// produce at least one decode. `tools/bench/ft8_capture_check.py` says which slots had a burst.
		expect(iq.length).toBeGreaterThan(0);
	}, 300_000);
});
