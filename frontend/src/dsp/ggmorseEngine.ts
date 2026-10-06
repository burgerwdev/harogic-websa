// The CW engine: ggmorse (MIT, https://github.com/ggerganov/ggmorse) compiled to wasm.
//
// Why ggmorse and not a hand-written slicer: a Morse decoder is a joint problem - find the tone,
// find the speed, find the character boundaries, and do all three on a signal that is fading, noisy
// and (on a bench with an unlocked reference) drifting. ggmorse solves it with a Goertzel bank plus
// a search over frames, auto-detecting pitch (0.2-1.2 kHz) and speed (5-55 WPM); measured on this
// machine it costs 2 ms per 100 ms of audio (worst 13 ms), so it runs inline in the audio worker.
//
// The module is built by tools/vendor/build_ggmorse_wasm.sh into `ggmorse/ggmorse.js` (a single ~70 KB file
// with the wasm embedded) and committed, like the DSP cores. See tools/vendor/ggmorse/ for the vendored
// sources and its MIT LICENSE.
//
// What this wrapper adds to the C API: float samples in (the pipeline's format), text chunks out,
// and the line breaks the decode window needs - ggmorse reports characters, not transmissions, so a
// long enough silence in the audio is what ends a line here.
import createGgMorse, { type GgMorseModule } from './ggmorse/ggmorse.js';

/** Bytes of text one take can return (a burst of characters after a gap). */
const TEXT_CAPACITY = 512;
/** Samples per call into wasm: 100 ms at 48 kHz, the audio worker's own block size. */
const CHUNK_SAMPLES = 4800;
/** No *trustworthy* character for this long ends the transmission: the window starts a new line.
 *
 * Three rules were tried on the bench and only the third works, which is worth recording:
 *
 *   * an audio level cannot do it - the chain's AGC levels noise and signal alike;
 *   * "no tone for N seconds" cannot either: a bench has a steady carrier somewhere in the passband
 *     (the transmitter's own LO leakage here), so the tone never stops and no transmission ever ended
 *     (reported: every transmission ran into one line);
 *   * "no character for N seconds" fails on its own because ggmorse emits stray characters out of a
 *     silence - its reporting lag reaches 2.2 s at 12 WPM - and each stray reset the timer.
 *
 * Counting only characters the tone gate stands behind (`share` at or above its threshold) settles it:
 * a stray from silence reads doubtful and cannot hold a line open, while a real message keeps it open
 * through its own word gaps. Four seconds is comfortably past any word gap, even at 10 WPM. */
const TRANSMISSION_GAP_S = 4.0;
/// Candidate sidetone frequencies the tone gate probes: every 50 Hz across the CW audio range, so a
/// tone is never more than 25 Hz off the nearest probe (the operator tunes the Pitch by ear).
const TONE_PROBE_HZ: number[] = [];
for (let hz = 250; hz <= 1600; hz += 50) TONE_PROBE_HZ.push(hz);
/// The narrow band's amplitude as a fraction of the audio's RMS at which a tone counts as present.
/// The two cases sit far apart, whatever the filter: a sidetone reads 0.71 (|LP| = A/2 against
/// rms = A/sqrt(2)) and loses at most 0.89 to landing between probes, so ~0.63, while noise of the
/// width the band was taken from reads sqrt(1.57 * 50 Hz / width) - 0.31 at 800 Hz, 0.16 at 3 kHz.
/// The threshold sits between them with room on both sides.
const TONE_MIN_SHARE = 0.45;
/// The gate's analysis bandwidth (one pole, so ~50 Hz of equivalent noise bandwidth): wide enough to
/// keep a tone that lands between probes, narrow enough that noise barely fills it.
const TONE_BANDWIDTH_HZ = 50;
/// The band-pass the decoder's audio is put through first, half-width in Hz (a one-pole around the
/// operator's Pitch, so the passband is +/- this).
///
/// The IF filter the operator chose is what protects the *demodulator*, and it is deliberately wider
/// than CW needs (3 kHz for a forgiving tune): the decoder then sees every carrier and all the noise
/// inside it, which is what its wrong characters come from - they carry a *high* tone share, so they
/// are misread keying, not noise (measured: the passband of the bench held dozens of comparable
/// carriers).
///
/// ggmorse band-passes to the Pitch itself, but only *after* estimating the level, so out-of-band
/// carriers still skew that estimate; filtering first is what measurably helped (bench, one
/// transmission per 15 s: the share of lines that opened with garbage fell from most of them to one in
/// five). The width is generous for the same reason ggmorse's own range is: the operator tunes the
/// Pitch by ear and a real link is always a little off.
const DECODE_BANDWIDTH_HZ = 200;
/// The gate's smoothing (one pole on the band's envelope and on the audio's level).
///
/// The estimate has to be *integrated*, not sampled: a single 100 ms look at one narrow band is a
/// noisy estimate, and taking its best over ~28 bands made band-limited noise spike past the
/// threshold (measured: 800 Hz-wide noise peaked at 0.52 with a raw estimate, and crossed it often
/// enough to keep decoding characters). Smoothed over ~50 ms the same noise tops out at 0.39 while a
/// keyed sidetone stays above 0.5 (measured: keyed 15/25 WPM 0.63/0.60, tones 0.71).
const TONE_SMOOTH_HZ = 3;
/// How long a character is still accepted after the tone disappears: ggmorse reports a character
/// just after its keying ends, so the gate must not close on the same block.
const TONE_HOLD_S = 0.8;
/// How long the gate's share is held at its peak for the character-confidence proxy.
///
/// Judged on the instantaneous share, correct characters came out marked as doubtful (reported, with
/// a screenshot): a character is reported *after* its keying ends, and a character that starts a word
/// is reported after the word gap, by which time the narrow band has long decayed - 0.4 s of hold
/// still read 0.33 for the character after a gap and underlined the start of every word. Two seconds
/// keeps the keying that produced the character in view through any word gap while still letting a
/// stray character from a long silence read as the guess it is.
const SHARE_PEAK_HOLD_S = 2.0;
/// The gate runs on 6:1 decimated audio. It only looks at 250-1600 Hz, so 8 kHz of rate is plenty
/// (Nyquist 4 kHz) and the boxcar that does the decimation also keeps the out-of-band noise from
/// folding back onto the probes: 28 oscillators per sample at 48 kHz made the engine's own test time
/// out under load (measured), and the decision is identical at a sixth of the work.
const TONE_DECIM = 6;

export interface CwChunk {
	text: string;
	/** True when this chunk closes a transmission (a long silence followed it). */
	endsLine: boolean;
	/** How much of the level sat in the narrowest band at this moment (0..1): the confidence proxy
	 * the window renders with (ggmorse itself reports no per-character confidence). */
	share: number;
	/** The audio's level (rms), for the window's meter. */
	rms: number;
	/** A sidetone is arriving *right now* (no hold): what a "keyed" lamp shows. */
	keyed: boolean;
}

export class GgMorseEngine {
	/**
	 * One sample through the decoder's input band-pass: mix down by the Pitch, one pole, mix back up.
	 *
	 * A real band-pass around the operator's Pitch rather than a low-pass, because that is where a CW
	 * sidetone lives (the operator tunes the Pitch by ear, so a few hundred Hz of width is all the
	 * tolerance that is needed and all the noise it lets through).
	 */
	private bandPass(x: number): number {
		this.bpI += this.bpAlpha * (2 * x * this.bpRe - this.bpI);
		this.bpQ += this.bpAlpha * (2 * x * this.bpIm - this.bpQ);
		const re = this.bpRe, im = this.bpIm;
		this.bpRe = re * this.bpDR - im * this.bpDI;
		this.bpIm = re * this.bpDI + im * this.bpDR;
		// Back up to the audio band: twice the real part of the analytic signal.
		return this.bpI * this.bpRe + this.bpQ * this.bpIm;
	}

	/**
	 * Is a sidetone present in this block?
	 *
	 * The discriminator has to be *spectral*: ggmorse decodes noise into characters quite happily (it has
	 * no squelch of its own), and the audio chain cannot tell the two apart because its AGC levels noise
	 * and signal alike. What separates them is how much of the level sits in one narrow band, which
	 * works at any filter width: a sidetone puts nearly everything there, noise spreads it over whatever
	 * filter the operator chose.
	 *
	 * A plain autocorrelation does *not* work here, and the reason is worth keeping: at 3 kHz of audio
	 * bandwidth noise at a half-millisecond delay is already ~0.8 correlated, so a correlation-only gate
	 * passes the very noise it was added to reject (measured: stray characters kept arriving with the
	 * transmitter off).
	 */
	private tonePresent(pcm: Float32Array): boolean {
		const a = this.bandAlpha;
		const b = this.envAlpha;
		const n = this.bandEnv.length;
		let box = 0;
		let boxN = 0;
		// The block's own peak, for the confidence proxy: a block spans keying and pauses, and the
		// value at its *end* is what the gate judges (measured behaviour), not what a character inside
		// it was keyed at. Without this, a character reported after a pause read as doubtful (measured
		// in the engine's own test: 0.14).
		let blockPeak = 0;
		for (let i = 0; i < pcm.length; i++) {
			box += pcm[i];
			if (++boxN < TONE_DECIM) continue;
			const x = box / TONE_DECIM;
			box = 0;
			boxN = 0;
			this.levelEma += b * (x * x - this.levelEma);
			for (let k = 0; k < n; k++) {
				this.bandI[k] += a * (x * this.ncoRe[k] - this.bandI[k]);
				this.bandQ[k] += a * (x * this.ncoIm[k] - this.bandQ[k]);
				const re = this.ncoRe[k], im = this.ncoIm[k];
				this.ncoRe[k] = re * this.ncoDR[k] - im * this.ncoDI[k];
				this.ncoIm[k] = re * this.ncoDI[k] + im * this.ncoDR[k];
				this.bandEnv[k] += b * (Math.hypot(this.bandI[k], this.bandQ[k]) - this.bandEnv[k]);
				if (this.levelEma > 0) {
					const sh = this.bandEnv[k] / Math.sqrt(this.levelEma);
					if (sh > blockPeak) blockPeak = sh;
				}
			}
		}
		const rms = Math.sqrt(this.levelEma);
		this.lastRms = rms;
		if (!(rms > 0)) {
			this.lastShare = 0;
			return false;
		}
		let best = 0;
		for (let k = 0; k < n; k++) {
			// A sinusoid of amplitude A reads |LP| ~ A/2 against an rms of A/sqrt(2): 0.71, while the
			// widest noise the chain can produce stays at 0.24-0.39 (measured, per filter width).
			const share = this.bandEnv[k] / rms;
			if (share > best) best = share;
		}
		this.lastShare = Math.max(best, blockPeak);
		return best >= TONE_MIN_SHARE;
	}

	private textPtr = 0;
	/// Tone-gate state: a narrow-band oscillator and envelope per candidate pitch, and the audio's
	/// own smoothed level (see `tonePresent`).
	private readonly bandAlpha: number;
	private readonly envAlpha: number;
	private readonly ncoRe: Float64Array;
	private readonly ncoIm: Float64Array;
	private readonly ncoDR: Float64Array;
	private readonly ncoDI: Float64Array;
	private readonly bandI: Float64Array;
	private readonly bandQ: Float64Array;
	private readonly bandEnv: Float64Array;
	private levelEma = 0;
	/// The decoder's input band-pass state (see `DECODE_BANDWIDTH_HZ`).
	private readonly bpAlpha: number;
	private readonly bpDR: number;
	private readonly bpDI: number;
	private bpRe = 1;
	private bpIm = 0;
	private bpI = 0;
	private bpQ = 0;
	private lastShare = 0;
	private lastRms = 0;
	/** Decaying peak of the gate's share (see `sharePeakS`): the character-confidence proxy. */
	private sharePeak = 0;
	private inPtr = 0;
	private inCapacity = 0;
	/// Seconds since the last *trustworthy* character (see `TRANSMISSION_GAP_S`).
	private sinceConfidentS = 0;
	private toneHold = 0;
	private linePending = false;

	private constructor(
		private module: GgMorseModule,
		private handle: number,
		private sampleRate: number,
	) {
		// f64 arrays: the gate runs per sample over the whole audio stream, and f32 accumulators drift
		// visibly in a one-pole that never stops running.
		const gateRate = sampleRate / TONE_DECIM;
		this.bandAlpha = 1 - Math.exp(-2 * Math.PI * TONE_BANDWIDTH_HZ / gateRate);
		this.envAlpha = 1 - Math.exp(-2 * Math.PI * TONE_SMOOTH_HZ / gateRate);
		const n = TONE_PROBE_HZ.length;
		this.ncoRe = new Float64Array(n).fill(1);
		this.ncoIm = new Float64Array(n);
		this.ncoDR = new Float64Array(n);
		this.ncoDI = new Float64Array(n);
		this.bandI = new Float64Array(n);
		this.bandQ = new Float64Array(n);
		this.bandEnv = new Float64Array(n);
		for (let k = 0; k < n; k++) {
			const wk = 2 * Math.PI * TONE_PROBE_HZ[k] / gateRate;
			this.ncoDR[k] = Math.cos(wk);
			this.ncoDI[k] = Math.sin(wk);
		}
		// The input band-pass runs at the full rate, around the operator's Pitch.
		this.bpAlpha = 1 - Math.exp(-2 * Math.PI * DECODE_BANDWIDTH_HZ / sampleRate);
		const pw = 2 * Math.PI * this.pitchHz / sampleRate;
		this.bpDR = Math.cos(pw);
		this.bpDI = Math.sin(pw);
	}

	/**
	 * Instantiate the wasm module and create one decoder.
	 *
	 * `pitchHz` is the operator's Pitch control: ggmorse searches around it (+/- `toleranceHz`)
	 * rather than over the whole band, so a stronger carrier nearby cannot steal the lock.
	 */
	static async load(sampleRate = 48000, pitchHz = 700, toleranceHz = 250): Promise<GgMorseEngine> {
		const module = await createGgMorse();
		const create = module.cwrap('ggmorse_wasm_new', 'number', ['number', 'number', 'number']);
		const handle = create(sampleRate, pitchHz, toleranceHz);
		if (!handle) throw new Error('ggmorse: decoder allocation failed');
		const engine = new GgMorseEngine(module, handle, sampleRate);
		engine.textPtr = module._malloc(TEXT_CAPACITY);
		engine.inCapacity = CHUNK_SAMPLES;
		engine.inPtr = module._malloc(engine.inCapacity * 2);
		if (!engine.textPtr || !engine.inPtr) throw new Error('ggmorse: buffer allocation failed');
		return engine;
	}

	/** The pitch ggmorse settled on, in Hz (0 until it detects one). */
	get pitchHz(): number {
		return this.module.cwrap('ggmorse_wasm_pitch_hz', 'number', ['number'])(this.handle);
	}

	/** The speed ggmorse settled on, in WPM (0 until it detects one). */
	get wpm(): number {
		return this.module.cwrap('ggmorse_wasm_wpm', 'number', ['number'])(this.handle);
	}

	/**
	 * Feed demodulated audio (mono, +/-1) and take what it decoded.
	 *
	 * Returns null when there is nothing to report - the common case, since a character only appears
	 * when the whole of it has been keyed.
	 */
	push(pcm: Float32Array): CwChunk | null {
		if (!this.handle || pcm.length === 0) return null;
		const push = this.module.cwrap('ggmorse_wasm_push_i16', null, ['number', 'number', 'number']);
		const heap = this.module.HEAP16;
		let sum = 0;
		for (let at = 0; at < pcm.length; at += this.inCapacity) {
			const count = Math.min(this.inCapacity, pcm.length - at);
			const base = this.inPtr >> 1;
			for (let i = 0; i < count; i++) {
				const v = this.bandPass(pcm[at + i]);
				sum += v * v;
				heap[base + i] = v >= 1 ? 32767 : v <= -1 ? -32768 : Math.round(v * 32767);
			}
			push(this.handle, this.inPtr, count);
		}
		// The line break is ours to find: the engine reports characters, not transmissions.
		// The tone gate (see `tonePresent`): it is what keeps noise from being decoded as text.
		const keyed = this.tonePresent(pcm);
		const blockS = pcm.length / this.sampleRate;
		this.sharePeak = Math.max(this.lastShare, this.sharePeak * Math.exp(-blockS / SHARE_PEAK_HOLD_S));
		if (keyed) {
			this.toneHold = Math.round(this.sampleRate * TONE_HOLD_S);
		} else if (this.toneHold > 0) {
			this.toneHold = Math.max(0, this.toneHold - pcm.length);
		}

		const take = this.module.cwrap('ggmorse_wasm_take_text', 'number', ['number', 'number', 'number']);
		const bytes = take(this.handle, this.textPtr, TEXT_CAPACITY);
		let text = '';
		if (bytes > 0) {
			const view = this.module.HEAPU8.subarray(this.textPtr, this.textPtr + bytes);
			// Control characters are noise here (ggmorse emits a stray newline when it starts).
			text = new TextDecoder().decode(view).replace(/[\u0000-\u001f]+/g, '');
			// No tone, no text: ggmorse decodes noise into characters quite happily (measured on the
			// bench with the transmitter off), and the audio chain cannot tell the two apart because
			// its AGC levels noise and signal alike.
			if (text && this.toneHold <= 0) text = '';
			if (text) this.linePending = true;
		}
		// The end of a transmission is the absence of a *trustworthy* character (see
		// `TRANSMISSION_GAP_S`): a stray ggmorse emits out of a silence must not hold the line open, and
		// neither must a steady carrier in the passband keep it alive forever.
		if (text && this.sharePeak >= TONE_MIN_SHARE) this.sinceConfidentS = 0;
		else this.sinceConfidentS += blockS;
		const endsLine = this.linePending && this.sinceConfidentS >= TRANSMISSION_GAP_S;
		if (endsLine) this.linePending = false;
		// Always a chunk: the text may be empty while the level, the gate state and the confidence
		// still describe the block (the window's meter and lamp run off them).
		return {
			text, endsLine,
			share: this.sharePeak, rms: this.lastRms, keyed,
		};
	}

	free(): void {
		if (!this.module) return;
		this.module.cwrap('ggmorse_wasm_free', null, ['number'])(this.handle);
		this.handle = 0;
		if (this.textPtr) this.module._free(this.textPtr);
		if (this.inPtr) this.module._free(this.inPtr);
		this.textPtr = 0;
		this.inPtr = 0;
	}
}
