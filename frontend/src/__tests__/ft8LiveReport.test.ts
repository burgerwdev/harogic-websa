// Live-capture acceptance: the same decode path as ft8Capture.test.ts, with the per-slot report
// written to a FILE (`WEBSA_FT8_OUT`, default /tmp/ft8_live_decodes.txt) instead of the console --
// machine-readable, so an acceptance run's decode count can be compared against another
// receiver's (the FT8CN phone app hears the same audio) without scraping test output.
//
//   python3 tools/ft8_capture_check.py --frequency 7.074e6 --seconds 150
//   WEBSA_FT8_IQ=/tmp/ft8_capture.json npx vitest run src/__tests__/ft8LiveReport.test.ts
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { describe, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
// skipIf below guards the unset case; the assertion is what keeps tsc quiet past it.
const capturePath = process.env.WEBSA_FT8_IQ!;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe.skipIf(!capturePath)('live decode report', () => {
	it('decodes and reports to a file', async () => {
		const meta = JSON.parse(readFileSync(capturePath, 'utf8'));
		const bytes = readFileSync(resolve(dirname(capturePath), meta.iq_file));
		const iq = new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));

		const params: PipelineParams = {
			fsIn: meta.fs_in,
			outRate: meta.out_rate,
			mode: meta.mode,
			ifBw: meta.if_bw ?? 2400,
			pitch: meta.pitch ?? 700,
			deemphUs: -1,
		};
		const module = await instantiateDsp(artifactBytes());
		const pipeline = new WasmPipeline(module, params, true);
		const block = 4_096;
		// A live stream continues past the capture end; a finite replay that stops just short of a
		// decode trigger would misrepresent it. Pad a quarter second of the same silence.
		const padded = new Float32Array(iq.length + Math.round(meta.out_rate * 0.3) * 2);
		padded.set(iq);
		const fed = padded;
		const lines: string[] = [];
		let blocks = 0;
		for (let start = 0; start + block * 2 <= fed.length; start += block * 2) {
			const reports = pipeline.push(fed.subarray(start, start + block * 2));
			blocks += 1;
			for (const message of reports) {
				const t = ((blocks * block) / meta.out_rate / 15).toFixed(0);
				lines.push(`slot ${t}: ${message.text} @ ${message.frequencyHz.toFixed(0)} Hz snr ${message.snrDb.toFixed(0)} dB`);
			}
		}
		pipeline.free();
		lines.push(`TOTAL: ${blocks} blocks (${(fed.length / 2 / meta.fs_in).toFixed(1)} s incl. pad), ${lines.length} decodes`);
		writeFileSync(process.env.WEBSA_FT8_OUT ?? '/tmp/ft8_live_decodes.txt', lines.join('\n') + '\n');
	}, 600_000);
});
