/**
 * Long-run parity: a captured baseband through the shipped artifact must not drift.
 *
 * The committed parity fixtures are short synthetic blocks, so they cannot see a gain that walks, an
 * AGC that pumps, or a state that accumulates. This feeds a *real* capture - 46 s of analyzer noise,
 * written by `tools/bench/ft8_capture_check.py` - and asserts the one property such a defect breaks: the
 * output level of a stationary input stays put, second by second.
 *
 *   python3 tools/bench/ft8_capture_check.py --frequency 21.074e6 --seconds 45 --out /tmp/nb.iq.json
 *   WEBSA_NB_IQ=/tmp/nb.iq.json npx vitest run src/__tests__/basebandParity.test.ts
 *
 * (A per-second bound, not a per-block one: the short-time RMS of any noise band fluctuates by several
 * percent, and reading that fluctuation as a defect is exactly the mistake this file exists to prevent.)
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const capturePath = process.env.WEBSA_NB_IQ;

describe.skipIf(!capturePath)('a long capture through the artifact', () => {
	for (const mode of ['usb', 'am', 'lsb']) {
		it(`${mode}: the level of a stationary input does not drift`, async () => {
			const meta = JSON.parse(readFileSync(capturePath!, 'utf8'));
			const bytes = readFileSync(resolve(dirname(capturePath!), meta.iq_file));
			const iq = new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
			const params: PipelineParams = {
				fsIn: meta.fs_in,
				outRate: meta.out_rate,
				mode,
				ifBw: meta.if_bw,
				pitch: 700,
				deemphUs: -1,
			};
			const module = await instantiateDsp(
				(readFileSync(resolve(process.cwd(), 'public', 'dsp.wasm')).buffer as ArrayBuffer).slice(0),
			);
			const pipeline = new WasmPipeline(module, params, false);
			const block = 4_096;
			const pcm: number[] = [];
			for (let start = 0; start + block * 2 <= iq.length; start += block * 2) {
				const out = pipeline.process(iq.subarray(start, start + block * 2));
				for (let i = 0; i < out.length; i++) pcm.push(out[i]);
			}
			pipeline.free();

			const second = Math.round(meta.out_rate);
			const levels: number[] = [];
			// The first couple of seconds are the AGC's priming transient and its slow release, by
			// design (AM measured 0.074 -> 0.088 over the first two seconds, then flat).
			for (let start = 2 * second; start + second <= pcm.length; start += second) {
				let sum = 0;
				for (let i = start; i < start + second; i++) sum += pcm[i] * pcm[i];
				levels.push(Math.sqrt(sum / second));
			}
			expect(levels.length).toBeGreaterThan(10);
			// Against the median, not min/max: the AGC's own settling is one tail of the distribution
			// (AM rose from 0.074 to 0.088 across its first seconds), and what this guards against is a
			// gain that keeps walking - which moves the later seconds away from the middle.
			const sorted = [...levels].sort((a, b) => a - b);
			const median = sorted[sorted.length >> 1];
			expect(Math.min(...levels)).toBeGreaterThan(median * 0.8);
			expect(Math.max(...levels)).toBeLessThan(median * 1.2);
			// And the level is the AGC's target, not something that walked away from it.
			const mean = levels.reduce((a, b) => a + b, 0) / levels.length;
			expect(Math.abs(mean - 0.1)).toBeLessThan(0.03);
		});
	}
});
