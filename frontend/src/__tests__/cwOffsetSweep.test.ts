/**
 * CW frequency tolerance: the permitted distance between the received sidetone and the operator's
 * Pitch, and the operation of the decode band-pass.
 *
 * This test reproduces the reported fault. On the bench, the clocks of the two radios put the tone
 * a few hundred Hz from the Pitch. A person heard correct CW audio. An external phone application,
 * which finds the pitch automatically, decoded the signal. This engine decoded nothing. Two causes
 * were measured and corrected:
 *
 *   * the tone search stayed at `Pitch +/- 250` (ggmorse_wasm.cpp). This value caused a sharp
 *     limit: the decoder decoded no character at 300 Hz and more from the Pitch (see
 *     `CW_SEARCH_HZ`);
 *   * the "band-pass at the Pitch" used the ggmorse *estimate* of the pitch, and this value is 0
 *     until audio arrives. Thus the mixer did not rotate, and the filter was a low-pass at DC.
 *     Measured gain: -5.2 dB at 700 Hz, and -9.7 dB at 1200 Hz. Each real sidetone had this loss
 *     (see `recenter`).
 *
 * The sweep is the acceptance condition. Each offset in the CW audio band (a sidetone of 250 Hz to
 * 1500 Hz) must give the complete message.
 */
import { describe, expect, it } from 'vitest';
import { GgMorseEngine } from '../dsp/ggmorseEngine';

const FS = 48000;
/** The operator's Pitch control. The engine uses this value. */
const PITCH = 800;
const TEXT = 'CQ DE W1ABC';
const WPM = 20;

/** The characters that the sweep sends. */
const CODE: Record<string, string> = {
	A: '.-', B: '-...', C: '-.-.', D: '-..', E: '.', G: '--.', H: '....', K: '-.-',
	L: '.-..', N: '-.', O: '---', Q: '--.-', S: '...', T: '-', W: '.--', '0': '-----',
	'1': '.----', '4': '....-',
};

interface Run { key: boolean; samples: number }

/** The keying of an ideal sender for `text` at `wpm`. The ratios are 1/3 dots and 1/3/7-dot gaps. */
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

/** Noise with a known sequence. Thus a comparison of two runs, and CI, uses the same samples. */
function mulberry32(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = Math.imul(a ^ (a >>> 15), 1 | a);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/** Two transmissions with the gap between them, and a tail. GGMorse examines recent audio. Thus a
 * message at the end of the recording has no audio for the decision. The tone is at `toneHz`. This
 * value is not necessarily the Pitch of the engine. */
function stream(toneHz: number): Float32Array {
	const runs = [...schedule(TEXT, WPM), { key: false, samples: Math.round(FS * 2.5) }];
	const all = [...runs, ...schedule(TEXT, WPM)];
	const total = all.reduce((n, r) => n + r.samples, 0) + Math.round(3.0 * FS) + Math.round(0.2 * FS);
	const out = new Float32Array(total);
	let at = Math.round(0.2 * FS);
	let phase = 0;
	for (const run of all) {
		for (let i = 0; i < run.samples; i++) {
			out[at] = run.key ? 0.4 * Math.sin(phase) : 0;
			phase += (2 * Math.PI * toneHz) / FS;
			at++;
		}
	}
	return out;
}

/** Band-limited noise at a target rms. This noise agrees with the output of the demodulator: the CW
 * audio is filtered to the selected IF bandwidth. Thus a 50 Hz probe contains sqrt(50/width) of the
 * noise, and not sqrt(50/24000). Full-band white noise is worse than the noise of a real receiver. */
function addBandNoise(pcm: Float32Array, noiseRms: number, widthHz: number, seed: number): void {
	const rnd = mulberry32(seed);
	const taps = Math.max(2, Math.round(FS / widthHz));
	const raw = new Float32Array(pcm.length);
	const out = new Float32Array(pcm.length);
	let acc = 0;
	for (let i = 0; i < pcm.length; i++) {
		raw[i] = rnd() * 2 - 1;
		acc += raw[i];
		if (i >= taps) acc -= raw[i - taps];
		out[i] = acc / taps;
	}
	let sum = 0;
	for (let i = 0; i < out.length; i++) sum += out[i] * out[i];
	const scale = noiseRms / Math.sqrt(sum / out.length);
	for (let i = 0; i < pcm.length; i++) pcm[i] += out[i] * scale;
}

/** The Levenshtein distance. The engine test explains why "nearly right" is the measure. */
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

/** The smallest number of errors between `expected` and a window of the decoded text with the same
 * length.
 *
 * A window is necessary, and not a line. The line breaks come from this code, because the engine
 * does not report a transmission boundary. In a noisy band, the two transmissions can be in one
 * line. The important property is the presence of the message, and not the position of the line
 * break. */
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

interface Decoded { text: string; engine: GgMorseEngine }

/** Push the audio in blocks of 100 ms, as the audio worker does. Return the engine also, thus a test
 * can read the values that the engine selected. */
async function decodeWith(pcm: Float32Array, pitch = PITCH): Promise<Decoded> {
	const engine = await GgMorseEngine.load(FS, pitch);
	const block = 4800;
	let text = '';
	for (let i = 0; i < pcm.length; i += block) {
		const chunk = engine.push(pcm.subarray(i, Math.min(i + block, pcm.length)));
		if (chunk) text += chunk.text;
	}
	return { text, engine };
}

async function decode(pcm: Float32Array, pitch = PITCH): Promise<string> {
	const { text, engine } = await decodeWith(pcm, pitch);
	engine.free();
	return text;
}

/** The sidetone frequencies that the decoder must follow (the CW audio band that the tone gate
 * examines), as offsets from the Pitch of the engine. A sidetone below 250 Hz or above 1500 Hz is
 * outside the range of the decoder (see `CW_SEARCH_HZ`). Thus these values are not part of the
 * acceptance condition. */
const TONE_BAND_HZ = { low: 250, high: 1500 };
const OFFSETS = [
	TONE_BAND_HZ.low - PITCH, -400, -300, -250, -150, -50,
	0, 50, 150, 250, 300, 400, 600,
	TONE_BAND_HZ.high - PITCH,
];

describe("the CW engine's frequency tolerance", () => {
	it('decodes the whole CW audio band with the sidetone off the Pitch', async () => {
		const rows: { offset: number; errors: number; text: string }[] = [];
		for (const offset of OFFSETS) {
			const text = await decode(stream(PITCH + offset));
			rows.push({ offset, errors: bestWindow(text, TEXT), text: text.trim().slice(0, 40) });
		}
		// A ggmorse decode is statistical. Therefore one stray character is permitted. The window
		// check counts the smallest number of errors against a window of the output with the same
		// length.
		for (const r of rows) {
			expect(r.errors, `tone ${PITCH + r.offset} Hz (Pitch ${PITCH} ${r.offset >= 0 ? '+' : ''}${r.offset}): ${JSON.stringify(r.text)}`)
				.toBeLessThanOrEqual(1);
		}
	}, 300_000);

	it('centres the decode band-pass on the Pitch, not on DC', async () => {
		// The old code used the ggmorse estimate of the pitch for the centre of the band-pass. This
		// value is 0 until audio is decoded. Thus the mixer did not rotate, and the "band-pass at the
		// Pitch" was a low-pass at DC. Measured gain: -5.2 dB at 700 Hz, -9.7 dB at 1200 Hz, and
		// -11.5 dB at 1500 Hz.
		const engine = await GgMorseEngine.load(FS, PITCH);
		expect(engine.centerHz).toBe(PITCH);
		engine.free();
	});

	it('locks the band-pass onto a sidetone that is off the Pitch (AFC)', async () => {
		// The band-pass keeps out-of-band noise away from ggmorse. Thus the band-pass must be on the
		// signal, and not at the Pitch of the operator.
		const tone = PITCH + 400;
		const { text, engine } = await decodeWith(stream(tone));
		expect(bestWindow(text, TEXT), JSON.stringify(text)).toBeLessThanOrEqual(1);
		expect(Math.abs(engine.centerHz - tone), `band-pass centre ${engine.centerHz} vs tone ${tone}`)
			.toBeLessThanOrEqual(100);
		engine.free();
	}, 120_000);

	it('still copies a weak sidetone that sits well off the Pitch', async () => {
		// The reported condition: the operator listens to a signal that is a long distance from the
		// Pitch, and the noise floor is high. Before the two corrections, this column was empty. The
		// search window did not contain the tone, and the band-pass at DC removed most of the signal.
		const pcm = stream(PITCH + 600);
		addBandNoise(pcm, 0.35, 3000, 1);        // approximately -2 dB in a 3 kHz channel: a weak signal, but sufficient
		const text = await decode(pcm);
		expect(text.trim(), 'a weak off-Pitch signal must still produce text').not.toBe('');
		expect(bestWindow(text, TEXT), JSON.stringify(text)).toBeLessThanOrEqual(8);
	}, 120_000);
});
