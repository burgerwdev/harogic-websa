/**
 * The playback processor's buffer policy (the last stage before the speaker).
 *
 * This is where a rate mismatch between the PCM producer (a worker, paced by the analyzer's packets)
 * and the audio clock is absorbed, and where the reported one-puff-per-second stutter came from. The
 * worklet is plain JS loaded by `addModule`, so the test stubs the two globals it needs and drives
 * the ring directly: a producer at a given rate, a sink at `sampleRate`, and the resulting fill,
 * underruns, drift ratio and dropped samples.
 */
import { beforeAll, describe, expect, it } from 'vitest';

type Processor = {
	new (): {
		enabled: boolean;
		available: number;
		underruns: number;
		slipped: number;
		ratio: number;
		prime: number;
		target: number;
		maxFill: number;
		push(samples: Float32Array): void;
		process(inputs: unknown, outputs: Float32Array[][]): boolean;
		reset(): void;
	};
};

const RATE = 48_000;
let ProcessorClass: Processor | null = null;

beforeAll(async () => {
	(globalThis as unknown as { sampleRate: number }).sampleRate = RATE;
	class FakePort {
		onmessage: ((event: { data: unknown }) => void) | null = null;
		postMessage(): void {}
		start(): void {}
	}
	(globalThis as unknown as { AudioWorkletProcessor: unknown }).AudioWorkletProcessor = class {
		port = new FakePort();
	};
	(globalThis as unknown as { registerProcessor: unknown }).registerProcessor = (
		_name: string,
		cls: unknown,
	) => {
		ProcessorClass = cls as Processor;
	};
	// The worklet is plain JS loaded by `audioWorklet.addModule` in the browser; the stub above
	// captures the class it registers.
	// @ts-expect-error - no type declarations for a worklet module (and none are wanted).
	await import('../audio/sdrAudioWorklet.js');
});

interface Outcome {
	underruns: number;
	slipped: number;
	ratio: number;
	fill: number;
	maxFillSeen: number;
}

/** Drive `seconds` of audio: the producer at `producerSps`, the sink at `sampleRate`. */
function run(producerSps: number, seconds = 6, block = 960): Outcome {
	const proc = new ProcessorClass!();
	proc.enabled = true;
	const out = new Float32Array(128);
	const outputs = [[out]];
	const blockBuffer = new Float32Array(block);
	let pushed = 0;
	let t = 0;
	let maxFillSeen = 0;
	const total = seconds * RATE;
	while (t < total) {
		while (Math.floor((producerSps * t) / RATE) - pushed >= block) {
			proc.push(blockBuffer);
			pushed += block;
		}
		proc.process(null, outputs);
		t += 128;
		if (proc.available > maxFillSeen) maxFillSeen = proc.available;
	}
	return {
		underruns: proc.underruns,
		slipped: proc.slipped,
		ratio: proc.ratio,
		fill: proc.available,
		maxFillSeen,
	};
}

describe('the SDR playback buffer', () => {
	it('plays a matched producer without an underrun and holds the target fill', () => {
		const r = run(RATE);
		expect(r.underruns).toBe(0);
		expect(r.slipped).toBe(0);
		expect(Math.abs(r.ratio - 1)).toBeLessThan(0.02);
		// The fill is steered to the target (250 ms) and stays there: that is the jitter budget.
		expect(r.fill).toBeGreaterThan(RATE * 0.15);
		expect(r.fill).toBeLessThan(RATE * 0.4);
	});

	it('absorbs a producer that runs fast by resampling, not by dropping samples', () => {
		// +2%: a real device/daemon mismatch (and the shape of the reported stutter when it was not
		// absorbed). Without the drift corrector the ring filled and the writer lapped the reader.
		const r = run(RATE * 1.02, 12);
		expect(r.underruns).toBe(0);
		expect(r.slipped).toBe(0);
		expect(r.ratio).toBeGreaterThan(1.005);
		expect(r.ratio).toBeLessThan(1.04);
		// The pitch is corrected rather than the buffer drained: the fill stays bounded.
		expect(r.fill).toBeLessThanOrEqual(r.maxFillSeen);
	});

	it('absorbs a producer that runs slow by resampling', () => {
		const r = run(RATE * 0.98, 12);
		expect(r.underruns).toBe(0);
		expect(r.ratio).toBeLessThan(0.995);
		expect(r.ratio).toBeGreaterThan(0.96);
	});

	it('keeps the latency bounded when the producer floods the ring', () => {
		// A burst far beyond the ceiling (a stalled worker catching up) must not become seconds of
		// delay: the oldest samples are dropped and the audio stays live.
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		proc.push(new Float32Array(RATE * 3));          // 3 s at once
		// Everything beyond the ceiling is dropped, and accounted as such.
		expect(proc.available).toBe(proc.maxFill);
		expect(proc.slipped).toBe(RATE * 3 - proc.maxFill);
		for (let i = 0; i < 400; i++) proc.process(null, [[out]]);
		expect(proc.available).toBeLessThanOrEqual(proc.maxFill);
	});

	it('drops everything on reset, so a retune does not replay the old station', () => {
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		proc.push(new Float32Array(RATE));              // one second offered
		// It holds one second only up to the ceiling; the rest was dropped as stale.
		expect(proc.available).toBe(proc.maxFill);
		expect(proc.slipped).toBe(RATE - proc.maxFill);
		proc.reset();
		expect(proc.available).toBe(0);
		// ...and it primes again from the new stream.
		proc.push(new Float32Array(proc.prime + 128).fill(0.5));
		const outputs = [[out]];
		for (let i = 0; i < 20; i++) proc.process(null, outputs);
		expect(proc.underruns).toBe(0);
		expect(out.some((v) => v !== 0)).toBe(true);
	});
});
