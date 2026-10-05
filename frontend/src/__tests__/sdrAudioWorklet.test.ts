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
		port: { onmessage: (event: { data: unknown }) => void };
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
		// A matched producer settles at the target fill, and the ratio ends within one step of 1: the
		// loop corrects the 0.05 s between the priming level and the target, then stops. (The pitch is
		// the thing being protected here: nothing keeps moving it.)
		expect(Math.abs(r.ratio - 1)).toBeLessThanOrEqual(0.002);
		expect(r.fill).toBeGreaterThan(RATE * 0.15);
		expect(r.fill).toBeLessThan(RATE * 0.45);
	});

	it('absorbs a small rate mismatch by resampling, without dropping a sample', () => {
		// A fraction of a percent is what is left after the backend measures the baseband rate it
		// actually delivers and the DSP is told the consumer's rate. That residual is small (measured
		// ~0.05%), and this is the range the corrector is for: the fill stays near its target and the
		// ratio ends within one step of the offset, having moved toward it and never past it.
		for (const offset of [1.001, 0.999]) {
			const r = run(RATE * offset, 200);
			expect(r.underruns).toBe(0);
			expect(r.slipped).toBe(0);
			expect(r.fill).toBeGreaterThan(RATE * 0.1);
			expect(r.fill).toBeLessThan(RATE * 0.6);
			// The loop is still walking toward the offset at this horizon, so the guarantee is only that
			// it has not run away: within a step of the offset, on the correct side or on its way there.
			expect(Math.abs(r.ratio - 1)).toBeLessThanOrEqual(Math.abs(offset - 1) + 0.0005 + 0.002);
		}
	});

	it('keeps a much wrong rate live without losing samples', () => {
		// Six times the residual the corrector is sized for (0.3%): the loop still tracks it and nothing
		// is dropped - the fill parks further from its target, which is the price of a proportional
		// correction that never modulates the pitch.
		for (const offset of [1.003, 0.997]) {
			const r = run(RATE * offset, 200);
			expect(r.underruns).toBe(0);
			expect(r.slipped).toBe(0);
			expect(r.fill).toBeGreaterThan(RATE * 0.1);
			expect(r.fill).toBeLessThan(RATE * 0.85);
			expect(Math.sign(r.ratio - 1)).toBe(Math.sign(offset - 1));
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

	it('re-corrects at most once per window, and only a step at a time (no pitch glide)', () => {
		// The correction is a pitch, so it must be a rare, small step rather than a continuous drag:
		// the ratio may change once per window and never within one, and a step is under 1.5 cents
		// (0.05%), which is why the listener hears no wander on a steady signal.
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		const outputs = [[out]];
		let pushed = 0;
		let t = 0;
		const changesAt: number[] = [];
		let last = proc.ratio;
		while (t < 120 * RATE) {
			while (Math.floor((RATE * 1.01 * t) / RATE) - pushed >= 960) {
				proc.push(new Float32Array(960));
				pushed += 960;
			}
			proc.process(null, outputs);
			t += 128;
			if (proc.ratio !== last) {
				changesAt.push(t);
				last = proc.ratio;
			}
		}
		// One change per 10 s window at most (120 s = 12 windows; the correction is far slower).
		expect(changesAt.length).toBeLessThanOrEqual(12);
		for (let i = 1; i < changesAt.length; i++) {
			expect((changesAt[i] - changesAt[i - 1]) / RATE).toBeGreaterThanOrEqual(10);
		}
		// Each step is a step, not a jump: the ratio walked toward the offset, and no further.
		expect(last).toBeGreaterThan(1);
		expect(last).toBeLessThanOrEqual(1.01 + 0.0005);
		expect(proc.underruns).toBe(0);
	});

	it('does not modulate the pitch on arrival jitter', () => {
		// The reported defect: the fill was steered by ordinary arrival jitter, so the resampling ratio
		// wandered and the audio went with it ("faster and slower"). Jitter that leaves the average rate
		// alone must not make the ratio wobble - it may sit a step or two from 1, in ONE direction, and
		// that is all.
		const proc = new ProcessorClass!();
		proc.enabled = true;
		const out = new Float32Array(128);
		const outputs = [[out]];
		let pushed = 0;
		let t = 0;
		let n = 0;
		const ratios: number[] = [];
		// An exactly matched producer that delivers everything it owes every other quantum: the
		// arrivals swing by a whole 20 ms block, the delivered rate is exact.
		while (t < 60 * RATE) {
			if (n % 2 === 0) {
				const owed = t - pushed;
				if (owed > 0) {
					proc.push(new Float32Array(owed));
					pushed += owed;
				}
			}
			proc.process(null, outputs);
			t += 128;
			n += 1;
			if (n % 40 === 0) ratios.push(proc.ratio);
		}
		// One direction only: a wobble is what the listener heard, and a proportional loop driven by
		// the instantaneous fill is exactly what produced it.
		let direction = 0;
		for (let i = 1; i < ratios.length; i++) {
			const delta = ratios[i] - ratios[i - 1];
			if (delta === 0) continue;
			if (direction === 0) direction = Math.sign(delta);
			expect(Math.sign(delta)).toBe(direction);
		}
		const total = Math.abs(ratios[ratios.length - 1] - ratios[0]);
		expect(total).toBeLessThanOrEqual(0.002);
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

	it('keeps complete DRM super-frame bursts without periodic dropouts', () => {
		const proc = new ProcessorClass!();
		proc.enabled = true;
		proc.port.onmessage({ data: { type: 'buffer-mode', mode: 'drm' } });
		const out = new Float32Array(128);
		const outputs = [[out]];
		// Measured from the block-fed HE-AAC fixture: first 0.8 s, then 1.2 s
		// every 1.2 s. A 0.9 s ring drops 0.3 s at every later burst.
		for (let t = 0; t < 12 * RATE; t += 128) {
			if (t === 0) proc.push(new Float32Array(Math.round(0.8 * RATE)).fill(0.5));
			if (t > 0 && t % Math.round(1.2 * RATE) < 128) {
				proc.push(new Float32Array(Math.round(1.2 * RATE)).fill(0.5));
			}
			proc.process(null, outputs);
		}
		expect(proc.slipped).toBe(0);
		expect(proc.underruns).toBe(0);
		expect(out.some((v) => v > 0)).toBe(true);
		// A repeated STATUS/configure must not empty the ring.
		const fill = proc.available;
		proc.port.onmessage({ data: { type: 'buffer-mode', mode: 'drm' } });
		expect(proc.available).toBe(fill);
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
