/**
 * Hardware-in-the-loop audio measurement (SAN-90 + TinySA).
 *
 * Skipped unless `WEBSA_HIL_IQ` points at a capture made by `tools/hil_audio_check.py`. The capture
 * is real IQ from the analyzer while the TinySA drives a known tone; this test runs it through the
 * *committed* `dsp.wasm` with the same wrapper the worker uses and measures the demodulated audio:
 * SINAD and THD at the mode's expected tone, plus the level.
 *
 * The bound is deliberately modest: it is a bench measurement of a real radio, not a simulation, so
 * it asserts "the signal is there, cleanly", and the printed numbers are what gets recorded.
 *
 *   python3 tools/hil_audio_check.py --mode cw
 *   WEBSA_HIL_IQ=/tmp/hil_iq.json npx vitest run src/__tests__/hil.test.ts
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';
import { WasmPipeline, type PipelineParams } from '../sdr/wasmPipeline';

const ARTIFACT = resolve(process.cwd(), '..', '..', 'frontend', 'modern', 'public', 'dsp.wasm');
const capturePath = process.env.WEBSA_HIL_IQ;

interface Capture {
	iq_file: string;
	samples: number;
	fs_in: number;
	center_hz: number;
	mode: string;
	if_bw: number;
	pitch: number;
	out_rate: number;
	decimate: number;
	expected_tone_hz?: number;
}

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

/**
 * Offset of the strongest tone in the capture, in Hz relative to the capture centre.
 *
 * The bench generator's frequency is not guaranteed to land exactly on the analyzer's centre (it
 * ended up 200 kHz low on the first measurement run), and the DDC's NCO is precisely what handles
 * that: the measurement finds the tone and mixes it to DC, which also exercises the NCO on real
 * data instead of assuming the tuning was perfect.
 */
function toneOffsetHz(iq: Int16Array, fsIn: number): number {
	// Contiguous samples only, and *never* decimated: taking every n-th sample aliases the spectrum
	// (the first version of this function did that and reported a 386 kHz offset for a tone that
	// was 200 kHz low, so the NCO mixed noise down and the measurement read as silence).
	const count = Math.min(4096, iq.length / 2);
	const re = new Float64Array(count);
	const im = new Float64Array(count);
	for (let k = 0; k < count; k++) {
		const w = 0.5 - 0.5 * Math.cos((2 * Math.PI * k) / count);
		re[k] = (iq[2 * k] / 32768) * w;
		im[k] = (iq[2 * k + 1] / 32768) * w;
	}
	const magnitudeAt = (hz: number): number => {
		let accRe = 0;
		let accIm = 0;
		for (let k = 0; k < count; k++) {
			const ph = (2 * Math.PI * hz * k) / fsIn;
			accRe += re[k] * Math.cos(ph) - im[k] * Math.sin(ph);
			accIm += re[k] * Math.sin(ph) + im[k] * Math.cos(ph);
		}
		return Math.sqrt(accRe * accRe + accIm * accIm);
	};
	// Coarse sweep over the captured span, then refine around the winner.
	let best = 0;
	let bestMagnitude = -1;
	const coarseStep = fsIn / 1024;
	for (let hz = -fsIn / 2; hz < fsIn / 2; hz += coarseStep) {
		const magnitude = magnitudeAt(hz);
		if (magnitude > bestMagnitude) {
			bestMagnitude = magnitude;
			best = hz;
		}
	}
	let fineStep = coarseStep / 50;
	let fineBest = best;
	for (let hz = best - coarseStep; hz <= best + coarseStep; hz += fineStep) {
		const magnitude = magnitudeAt(hz);
		if (magnitude > bestMagnitude) {
			bestMagnitude = magnitude;
			fineBest = hz;
		}
	}
	best = fineBest;
	fineStep /= 20;
	for (let hz = best - fineStep * 20; hz <= best + fineStep * 20; hz += fineStep) {
		const magnitude = magnitudeAt(hz);
		if (magnitude > bestMagnitude) {
			bestMagnitude = magnitude;
			best = hz;
		}
	}
	return best;
}

/**
 * Tone power and SINAD over a bounded analysis segment.
 *
 * The segment length matters: a Hann-windowed DFT over millions of samples has a main lobe far
 * narrower than any practical peak search resolution, so probing "the peak" a fraction of a Hz off
 * reads the tone as absent (the first version did exactly that and reported -3 dB SINAD for a
 * perfectly good AM tone). 16384 samples (0.34 s at 48 kHz) has a ~2.9 Hz bin, which tolerates the
 * search resolution; the tone is then measured at the best frequency in a band around the nominal.
 */
function measureTone(
	pcm: Float32Array,
	rate: number,
	nominalHz: number,
): { hz: number; amp: number; sinad: number } {
	const n = Math.min(16_384, pcm.length);
	const segment = pcm.subarray(pcm.length - n);
	const amplitude = (hz: number): number => {
		let re = 0;
		let im = 0;
		let wsum = 0;
		for (let k = 0; k < n; k++) {
			const w = 0.5 - 0.5 * Math.cos((2 * Math.PI * k) / n);
			const ph = (2 * Math.PI * hz * k) / rate;
			re += segment[k] * w * Math.cos(ph);
			im -= segment[k] * w * Math.sin(ph);
			wsum += w;
		}
		return (2 * Math.sqrt(re * re + im * im)) / wsum;
	};
	let best = nominalHz;
	let bestAmp = -1;
	for (let hz = nominalHz * 0.7; hz <= nominalHz * 1.3; hz += 1) {
		const amp = amplitude(hz);
		if (amp > bestAmp) {
			bestAmp = amp;
			best = hz;
		}
	}
	for (let hz = best - 1; hz <= best + 1; hz += 0.1) {
		const amp = amplitude(hz);
		if (amp > bestAmp) {
			bestAmp = amp;
			best = hz;
		}
	}
	const total = rms(segment);
	const tonePower = (bestAmp * bestAmp) / 2;
	const noisePower = Math.max(total * total - tonePower, 1e-20);
	return { hz: best, amp: bestAmp, sinad: 10 * Math.log10(tonePower / noisePower) };
}

const rms = (pcm: Float32Array): number => {
	let sum = 0;
	for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
	return pcm.length ? Math.sqrt(sum / pcm.length) : 0;
};

describe.skipIf(!capturePath)('hardware-in-the-loop audio', () => {
	it('demodulates the bench tone through the shipped WASM artifact', async () => {
		const meta = JSON.parse(readFileSync(capturePath!, 'utf8')) as Capture;
		const iqBytes = readFileSync(resolve(dirname(capturePath!), meta.iq_file));
		const iq = new Int16Array(iqBytes.buffer, iqBytes.byteOffset, iqBytes.byteLength / 2);

		const module = await instantiateDsp(artifactBytes());
		// The capture centre is the tuning reference: the analyzer was tuned to the generator, and the
		// demodulator's own band/discriminator handle whatever small offset remains. An automatic peak
		// search is available for a deliberately off-centre capture, but it must not be the default —
		// for an FM signal the strongest bin is a *sideband* (measured: +5 kHz on a 6 kHz-deviation
		// signal), and mixing by that detunes the channel by its own modulation.
		const offsetHz = process.env.WEBSA_HIL_OFFSET === 'auto' ? toneOffsetHz(iq, meta.fs_in) : 0;
		const params: PipelineParams = {
			fsIn: meta.fs_in,
			offsetHz,
			decimate: meta.decimate,
			outRate: meta.out_rate,
			mode: meta.mode,
			ifBw: meta.if_bw,
			pitch: meta.pitch,
		};
		const pipeline = new WasmPipeline(module, params);
		expect(pipeline.ok).toBe(true);
		// The audio chain can be switched off for a measurement: with it on, the chain's policy (the
		// notch, the denoiser) shapes the result, and separating the two is what localises a failure.
		if (process.env.WEBSA_HIL_NO_CHAIN === '1') pipeline.setAudioEnabled(false);

		const block = 8_192;
		const chunks: Float32Array[] = [];
		for (let start = 0; start + block * 2 <= iq.length; start += block * 2) {
			chunks.push(pipeline.process(iq.subarray(start, start + block * 2)));
		}
		pipeline.free();
		const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
		const pcm = new Float32Array(total);
		let offset = 0;
		for (const chunk of chunks) {
			pcm.set(chunk, offset);
			offset += chunk.length;
		}
		expect(pcm.length).toBeGreaterThan(meta.out_rate * 0.5);   // at least half a second of audio

		const rate = meta.out_rate;
		// The tone is measured where it actually is: the detector shifts the carrier to the pitch, and
		// any residual carrier offset moves the tone by the same amount. Its distance from the nominal
		// frequency is asserted, which is what proves it is the expected tone and not a noise peak.
		const expected = meta.expected_tone_hz ?? (meta.mode === 'cw' ? meta.pitch : 1_000);
		const measured = measureTone(pcm, rate, expected);
		const tone = measured.hz;
		const signal = measured.amp;
		const sinad = measured.sinad;
		const level = rms(pcm);
		const harmonics = [2, 3, 4].map((n) => measureTone(pcm, rate, tone * n));
		const thd = 20 * Math.log10(
			Math.sqrt(harmonics.reduce((sum, h) => sum + h.amp * h.amp, 0)) /
				Math.max(signal, 1e-12),
		);
		console.log(
			`HIL ${meta.mode} @ ${(meta.center_hz / 1e6).toFixed(4)} MHz (tone offset ` +
				`${(offsetHz / 1e3).toFixed(1)} kHz, chain ${process.env.WEBSA_HIL_NO_CHAIN === '1' ? 'off' : 'on'}): ` +
				`sidetone ${tone.toFixed(1)} Hz (nominal ${expected.toFixed(0)} Hz, ` +
				`offset ${(tone - expected).toFixed(1)} Hz), level ` +
				`${(20 * Math.log10(level + 1e-12)).toFixed(1)} dBFS, SINAD ${sinad.toFixed(1)} dB, ` +
				`THD ${thd.toFixed(1)} dB`,
		);

		// Bounds per mode, set from the bench and the Python reference on the *same* capture:
		//   AM  (1 kHz, 50 % depth):            browser 28.6 dB / THD -36.2 dB, reference 26.1 / -36.2
		//   NFM (1 kHz, 6 kHz dev, 25 kHz IF):  browser  9.1 dB / THD  -6.5 dB, reference  8.7 /  -6.5
		// The absolute SINAD is the combined phase noise of the TinySA and the analyzer synthesizer
		// (the reference shows the same), so these assert "as good as the reference" rather than "as
		// good as a lab generator"; the tone frequency is asserted tightly, because that is what the
		// DDC and the detector have to get right.
		expect(Math.abs(tone - expected)).toBeLessThan(0.05 * expected + 20);
		expect(signal).toBeGreaterThan(0.005);
		expect(sinad).toBeGreaterThan(meta.mode === 'am' ? 15 : 5);
		// A full capture through the pipeline plus the DFT searches needs more than the 5 s default.
		expect(thd).toBeLessThan(meta.mode === 'am' ? -20 : -5);
	}, 180_000);
});
