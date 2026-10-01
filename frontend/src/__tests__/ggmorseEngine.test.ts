/**
 * The CW engine (ggmorse in wasm): synthetic Morse in, text out.
 *
 * This drives the real artifact the browser loads - the committed wasm module, the wrapper's C API
 * and the engine's PCM/line-break handling - so it fails if the build script, the wrapper or the
 * integration changes shape.
 *
 * The audio is a *stream*, like the radio: the message is sent twice with a gap between, and the
 * assertion is that the message comes out whole at least once. Cutting the stream right after the
 * last character loses it - ggmorse analyses the recent audio in a window of up to 3 s, so a message
 * that ends the recording has nothing left to resolve against (measured: 'N0CAL' with a 2 s tail).
 * A receiver never sees that; a test that did would be testing the truncation, not the decoder.
 */
import { describe, expect, it } from 'vitest';
import { GgMorseEngine } from '../dsp/ggmorseEngine';

const FS = 48000;
const PITCH = 800;

/** The characters these tests send. */
const CODE: Record<string, string> = {
	A: '.-', B: '-...', C: '-.-.', D: '-..', E: '.', G: '--.', H: '....', K: '-.-',
	L: '.-..', N: '-.', O: '---', Q: '--.-', S: '...', T: '-', W: '.--', '0': '-----',
	'1': '.----', '4': '....-',
};

interface Run { key: boolean; samples: number }

/** The keying an ideal sender produces for `text` at `wpm` (1/3 dots, 1/3/7-dot gaps). */
function schedule(text: string, wpm: number): Run[] {
	const dot = 1.2 / wpm;
	const s = (dots: number) => Math.round(dots * dot * FS);
	const runs: Run[] = [];
	const words = text.trim().split(/\s+/);
	words.forEach((word, wi) => {
		[...word].forEach((ch, ci) => {
			const code = CODE[ch] ?? '';
			[...code].forEach((sym, si) => {
				runs.push({ key: true, samples: s(sym === '.' ? 1 : 3) });
				if (si < code.length - 1) runs.push({ key: false, samples: s(1) });
			});
			if (ci < word.length - 1) runs.push({ key: false, samples: s(3) });
		});
		if (wi < words.length - 1) runs.push({ key: false, samples: s(7) });
	});
	runs.push({ key: false, samples: s(10) });      // the pause after a transmission
	return runs;
}

/** Two transmissions with the gap between them, plus hiss and a tail (see the module note). */
function stream(text: string, wpm: number, noise = 0, tailS = 3.0): Float32Array {
	// A 2.5 s pause between the two transmissions: longer than the engine's line break, and like the
	// silence between real slots.
	const runs = [...schedule(text, wpm), { key: false, samples: Math.round(FS * 2.5) }];
	const all = [...runs, ...schedule(text, wpm)];
	const total = all.reduce((n, r) => n + r.samples, 0) + Math.round(tailS * FS) + Math.round(0.2 * FS);
	const out = new Float32Array(total);
	let at = Math.round(0.2 * FS);
	let phase = 0;
	for (const run of all) {
		for (let i = 0; i < run.samples; i++) {
			out[at] = run.key ? 0.4 * Math.sin(phase) : 0;
			phase += (2 * Math.PI * PITCH) / FS;
			at++;
		}
	}
	if (noise > 0) {
		for (let i = 0; i < total; i++) out[i] += (Math.random() * 2 - 1) * noise * 0.5;
	}
	return out;
}

interface Decoded { text: string; lines: number; wpm: number }

/** Levenshtein distance, for the decodes that are *nearly* right: ggmorse is a statistical decoder,
 * and its published figures are error rates, not perfection (measured here: exact at 12-25 WPM,
 * one character or one trailing artifact at 35 WPM). */
function charErrors(actual: string, expected: string): number {
	const a = actual.replace(/\s+/g, ' ').trim();
	const b = expected;
	const rows = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
	for (let j = 0; j <= b.length; j++) rows[0][j] = j;
	for (let i = 1; i <= a.length; i++) {
		for (let j = 1; j <= b.length; j++) {
			rows[i][j] = Math.min(
				rows[i - 1][j] + 1,
				rows[i][j - 1] + 1,
				rows[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1),
			);
		}
	}
	return rows[a.length][b.length];
}

/** Push the audio in 100 ms blocks the way the audio worker does. */
async function decode(pcm: Float32Array, pitch = PITCH): Promise<Decoded> {
	const engine = await GgMorseEngine.load(FS, pitch);
	const block = 4800;
	let text = '';
	let lines = 0;
	const samples: number[] = [];
	for (let i = 0; i < pcm.length; i += block) {
		const chunk = engine.push(pcm.subarray(i, Math.min(i + block, pcm.length)));
		if (chunk) {
			text += chunk.text;
			if (chunk.endsLine) lines++;
		}
		// The speed estimate is only meaningful while a signal is being keyed (a long silence drags
		// it down, and the first frames spike), so the median of the non-zero samples is taken.
		const seen = engine.wpm;
		if (seen > 4) samples.push(seen);
	}
	engine.free();
	samples.sort((x, y) => x - y);
	return { text, lines, wpm: samples.length ? samples[Math.floor(samples.length / 2)] : 0 };
}

/** The fewest errors between `expected` and any same-length window of the decoded text.
 *
 * A window rather than a line, because the line breaks are ours (a transmission boundary is not
 * something the engine reports) and in a noisy band the two transmissions may well land in one
 * line - the property that matters is that the message is there, not where the line broke. */
function bestWindow(text: string, expected: string): number {
	const haystack = text.replace(/\s+/g, ' ').trim();
	let best = charErrors(haystack, expected);
	for (let len = expected.length - 3; len <= expected.length + 3; len++) {
		for (let at = 0; at + len <= haystack.length; at++) {
			best = Math.min(best, charErrors(haystack.slice(at, at + len), expected));
		}
	}
	return best;
}

describe('the CW engine (ggmorse)', () => {
	it('decodes a clean call and reports the speed', async () => {
		const { text, wpm } = await decode(stream('CQ DE W1ABC', 20));
		expect(bestWindow(text, 'CQ DE W1ABC'), JSON.stringify(text)).toBe(0);
		expect(wpm).toBeGreaterThan(17);
		expect(wpm).toBeLessThan(23);
	});

	// Four speeds, each a ~10 s stream through the wasm decoder: on a loaded CI machine this is well
	// past the default 5 s, so it carries its own budget.
	it('follows the sender from 12 to 35 WPM', async () => {
		for (const wpm of [12, 18, 25, 35]) {
			const { text, wpm: reported } = await decode(stream('TEST DE N0CALL', wpm));
			// A statistical decoder: exact at the speeds the bench uses, and within a character or two
			// (plus the occasional stray one after a transmission) at the extremes.
			expect(bestWindow(text, 'TEST DE N0CALL'), `${wpm} WPM: ${JSON.stringify(text)}`)
				.toBeLessThanOrEqual(2);
			expect(reported, `${wpm} WPM speed estimate`).toBeGreaterThan(wpm * 0.8);
			expect(reported, `${wpm} WPM speed estimate`).toBeLessThan(wpm * 1.25);
		}
	}, 120_000);

	it('decodes through hiss', async () => {
		const { text } = await decode(stream('CQ CQ DE G4ABC', 22, 0.5));
		expect(bestWindow(text, 'CQ CQ DE G4ABC'), JSON.stringify(text)).toBeLessThanOrEqual(2);
	});

	it('finds the tone when the operator set the Pitch off by a little', async () => {
		// ggmorse searches around the Pitch: the two clocks' difference (100-240 Hz on this bench) is
		// inside the window, so a coarse Pitch setting is not a cliff.
		const { text } = await decode(stream('CQ DE W1ABC', 20), PITCH - 150);
		expect(bestWindow(text, 'CQ DE W1ABC'), JSON.stringify(text)).toBe(0);
	});

	it('reports what the window needs: confidence proxy, level, lamp and pauses', async () => {
		// The decode window renders three things ggmorse itself does not report: how certain each
		// character is (the gate's share is the proxy), a live level for the meter, and whether a
		// word-length pause preceded the characters (judged against the sender's dot length, so it
		// works at any speed). None of them may be invented: a character that got past the gate always
		// carries the gate's state, and every chunk carries a real level.
		const engine = await GgMorseEngine.load(FS, PITCH);
		const pcm = stream('CQ DE W1ABC', 20);
		const block = 4800;
		let texts = 0, keyed = 0, badShare = 0, loud = 0;
		const audibleShares: number[] = [];
		for (let i = 0; i < pcm.length; i += block) {
			const c = engine.push(pcm.subarray(i, Math.min(i + block, pcm.length)));
			if (!c) continue;
			if (c.share < 0 || c.share > 1.001) badShare++;
			if (c.rms > 0.05) loud++;
			if (c.keyed) keyed++;

			if (c.text) {
				texts++;
				// Only a character decoded from *audible* audio must not read doubtful. ggmorse also
				// emits trailing characters out of silence (a statistical decoder's habit), and those
				// reading as doubtful is exactly right: they are the ones the operator should not trust.
				if (c.rms > 0.05) audibleShares.push(c.share);
			}
		}
		engine.free();
		expect(texts).toBeGreaterThan(2);
		expect(badShare).toBe(0);
		expect(loud).toBeGreaterThan(2);
		expect(keyed).toBeGreaterThan(2);
		// A character that came out of real keying must not be *drawn* as doubtful: judged on the
		// instantaneous share it was (reported: correct characters came out underlined), because a
		// character is reported just after its keying ends, when the narrow band has decayed.
		expect(audibleShares.length).toBeGreaterThan(2);
		expect(Math.min(...audibleShares)).toBeGreaterThan(0.45);
	});

	it('decodes nothing from noise alone (the tone gate)', async () => {
		// Reported: small fluctuations produced stray characters that were never sent. ggmorse has no
		// squelch of its own, and the audio chain cannot supply one (its AGC levels noise and signal
		// alike), so the engine gates on the audio's *shape*: a tone repeats at its period, noise does
		// not. Without the gate this noise decodes into characters.
		const n = Math.round(6 * FS);
		const noise = new Float32Array(n);
		for (let i = 0; i < n; i++) noise[i] = (Math.random() * 2 - 1) * 0.3;
		const { text, wpm } = await decode(noise);
		expect(text, `noise decoded as ${JSON.stringify(text)} (wpm ${wpm})`).toBe('');
	});

	it('decodes nothing from band-limited noise (the tone gate at a real filter width)', async () => {
		// The first version of the gate tested the autocorrelation at a few lags, which cannot work:
		// with the 3 kHz filter this audio comes from, noise is already ~0.8 correlated at a
		// half-millisecond delay, so the gate passed the noise (measured on the bench with the
		// transmitter off: stray characters kept arriving). The share of the amplitude in one narrow
		// band is what actually separates a sidetone from noise, at any filter width.
		// The demodulator's audio noise is *flat* across the filter it was tuned to (the band-pass at
		// the sidetone, then the detector's low-pass), so a moving average is the honest model here:
		// a first-order low-pass would pile the energy up at DC and look tone-like to any gate.
		for (const width of [800, 2000, 3000]) {
			const n = Math.round(6 * FS);
			const noise = new Float32Array(n);
			const taps = Math.max(2, Math.round(FS / width));
			let sum = 0;
			for (let i = 0; i < n; i++) {
				sum += (Math.random() * 2 - 1) * 0.5;
				if (i >= taps) sum -= noise[i - taps] * taps;   // running sum, rescaled below
				noise[i] = sum / taps;
			}
			// Rebuild as a proper moving average (the running sum above mis-scales the tail).
			for (let i = 0; i < n; i++) noise[i] = 0;
			let acc = 0;
			const raw = new Float32Array(n);
			for (let i = 0; i < n; i++) raw[i] = (Math.random() * 2 - 1) * 0.5;
			for (let i = 0; i < n; i++) {
				acc += raw[i];
				if (i >= taps) acc -= raw[i - taps];
				noise[i] = (acc / taps) * 3;
			}
			const { text } = await decode(noise);
			expect(text, `noise ${width} Hz wide decoded as ${JSON.stringify(text)}`).toBe('');
		}
	});

	it('ends a line on the pause between transmissions', async () => {
		const { text, lines } = await decode(stream('CQ DE W1ABC', 20));
		expect(bestWindow(text, 'CQ DE W1ABC'), JSON.stringify(text)).toBe(0);
		expect(lines, 'the pause between transmissions closes the line').toBeGreaterThanOrEqual(1);
	});
});
