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
	it('plays a matched producer without an underrun and holds its fill', () => {
		const r = run(RATE, 30);
		expect(r.underruns).toBe(0);
		expect(r.slipped).toBe(0);
		// A matched producer leaves the fill where it started playing (the priming threshold), and
		// nothing walks it away: no correction is needed, so the ratio stays exactly 1 (the pitch is
		// the thing being protected here).
		expect(r.ratio).toBe(1);
		expect(r.fill).toBeGreaterThan(RATE * 0.2);
		expect(r.fill).toBeLessThan(RATE * 0.4);
	});

	it('absorbs a small rate mismatch by resampling, without dropping a sample', () => {
		// A fraction of a percent is what is left after the backend reports the baseband rate it
		// actually delivers and the DSP is told the consumer's rate: that is what this corrector is
		// for. It measures once per window and steps once, so no sample is dropped and the pitch
		// moves by the true offset rather than wobbling.
		for (const offset of [1.003, 0.997]) {
			const r = run(RATE * offset, 60);
			expect(r.underruns).toBe(0);
			expect(r.slipped).toBe(0);
			expect(Math.abs(r.ratio - offset)).toBeLessThan(0.002);
		}
	});

	it('does not pretend to absorb a rate that is simply wrong', () => {
		// A 2% mismatch cannot be buffered away: the fill walks one window's worth of it (~960 ms at
		// 20 s), which the ceiling trims (fast producer) or the priming threshold starves (slow one).
		// That is why the backend *measures* the baseband rate instead of trusting the vendor's
		// nominal figure - the buffer is the last resort, not the mechanism.
		const fast = run(RATE * 1.02, 60);
		expect(fast.slipped).toBeGreaterThan(0);
		expect(fast.fill).toBeLessThanOrEqual(fast.maxFillSeen);
		const slow = run(RATE * 0.98, 60);
		expect(slow.underruns).toBeGreaterThan(0);
	});

	it('re-corrects at most once per window (no pitch glide)', () => {
		// The correction is a pitch, so it must be a rare step rather than a continuous drag: over a
		// minute at a 1% offset the ratio may change twice (2 windows), not 240 times (the report
		// cadence a proportional loop used).
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		const outputs = [[out]];
		let pushed = 0;
		let t = 0;
		const changes: number[] = [];
		let last = proc.ratio;
		while (t < 60 * RATE) {
			while (Math.floor((RATE * 1.01 * t) / RATE) - pushed >= 960) {
				proc.push(new Float32Array(960));
				pushed += 960;
			}
			proc.process(null, outputs);
			t += 128;
			if (proc.ratio !== last) {
				changes.push(proc.ratio);
				last = proc.ratio;
			}
		}
		expect(changes.length).toBeLessThanOrEqual(3);
		expect(last).toBeGreaterThan(1.005);
		expect(proc.underruns).toBe(0);
	});

	it('does not modulate the pitch on jitter', () => {
		// The reported defect: the fill was steered proportionally, so ordinary arrival jitter moved
		// the resampling ratio and the audio wandered (the listener heard "faster and slower"). The
		// ratio must hold still while the fill stays inside the deadband.
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		const outputs = [[out]];
		let pushed = 0;
		let t = 0;
		const ratios: number[] = [];
		// A matched producer that delivers in bursts of two blocks every other quantum: the fill
		// swings, the average rate is exact.
		while (t < 20 * RATE) {
			const owed = Math.floor((RATE * t) / RATE) - pushed;
			if ((t / 128) % 2 === 0 && owed < 1920) {
				proc.push(new Float32Array(960));
				pushed += 960;
			}
			proc.process(null, outputs);
			t += 128;
			if ((t / 128) % 40 === 0) ratios.push(proc.ratio);
		}
		const spread = Math.max(...ratios) - Math.min(...ratios);
		expect(spread).toBe(0);
		expect(proc.underruns).toBe(0);
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
