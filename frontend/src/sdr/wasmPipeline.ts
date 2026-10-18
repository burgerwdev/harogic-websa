// The WASM demodulator, wrapped for the worker.
//
// One handle per configured mode and one call per baseband block, for both paths: the analog one
// produces PCM, the digital one produces decoded text. The baseband is already channelized by the
// backend (the analyzer's DDC plus its tuning NCO), so what Rust owns here is the demodulator and
// the audio chain — not the channelizer. Nothing in this file knows the order of stages; it
// allocates two blocks, marshals the samples and reads the result back, which is the whole point of
// the raw-pointer ABI: no per-sample JavaScript work in the hot path.
//
// Views are created after every allocation (growing the memory detaches existing views, which
// reads as length 0 rather than throwing).
import type { DspModule } from './wasm';
import type { Ft8Report, PipelineParams, SstvDecoderCounters, SstvImageStatus, SstvPicture } from './types';

/** Blocks larger than this are truncated rather than reallocating in the audio path. */
const MAX_COMPLEX_SAMPLES = 32_768;
const TEXT_CAPACITY = 64;
/** Room for the largest SSTV picture: 800x616 (Wrasse SC2-180) is about 1.5 MB of RGB888. */
const IMAGE_CAPACITY = 2 * 1024 * 1024;
/** The picture state the ABI writes: width, height, lines, progress, vis, serial lo/hi, complete, mode. */
const IMAGE_WORDS = 9;
/** The decoder state the ABI writes: attempts lo/hi, buffered samples, has picture. */
const DECODE_STATE_WORDS = 5;
/** The rate the SSTV decoder runs its own buffer at (`DECODE_RATE_HZ` in the Rust plugin). */
const SSTV_DECODE_RATE_HZ = 8_000;

export type { Ft8Report, PipelineParams, SstvDecoderCounters, SstvImageStatus, SstvPicture };

export class WasmPipeline {
	private inPtr = 0;
	private outPtr = 0;
	private textPtr = 0;
	private metricsPtr = 0;
	private constPtr = 0;
	/** The picture's state words (`IMAGE_WORDS` u32) and its RGB888 pixels. */
	private imgMetaPtr = 0;
	private imgPtr = 0;
	private audioPtr = 0;
	/** The stereo difference signal's drain block. */
	private sidePtr = 0;
	private handle = 0;
	private volume = 1;

	params: PipelineParams;

	digital: boolean;

	constructor(private module: DspModule, params: PipelineParams, digital: boolean) {
		this.params = params;
		this.digital = digital;
		this.inPtr = module.alloc(MAX_COMPLEX_SAMPLES * 2 * 4);
		this.outPtr = module.alloc(MAX_COMPLEX_SAMPLES * 4);
		this.textPtr = module.alloc(TEXT_CAPACITY);
		this.metricsPtr = module.alloc(3 * 8);
		this.constPtr = module.alloc(2048 * 2 * 8);
		this.imgMetaPtr = module.alloc(IMAGE_WORDS * 4);
		this.imgPtr = module.alloc(IMAGE_CAPACITY);
		this.audioPtr = module.alloc(32768 * 2);
		this.sidePtr = module.alloc(MAX_COMPLEX_SAMPLES * 4);
		if (!this.inPtr || !this.outPtr || !this.textPtr || !this.metricsPtr || !this.constPtr
			|| !this.imgMetaPtr || !this.imgPtr || !this.audioPtr || !this.sidePtr) return;
		this.create();
	}

	/** True when the Rust side accepted the mode and geometry. */
	get ok(): boolean {
		return this.handle !== 0;
	}

	get mode(): string {
		return this.params.mode;
	}

	/** True for a protocol decoder (text out) rather than a demodulator (audio out). */
	get isDigital(): boolean {
		return this.digital;
	}

	/** The requested de-emphasis in microseconds (`< 0` = the mode's default). */
	get deemphUs(): number {
		return this.params.deemphUs;
	}

	private create(): void {
		const mode = new TextEncoder().encode(this.params.mode);
		const ptr = this.module.alloc(mode.length);
		if (!ptr) return;
		try {
			this.module.u8View(ptr, mode.length).set(mode);
			this.handle = this.module.exports.websa_dsp_demod_new(
				this.params.fsIn,
				this.params.outRate,
				ptr,
				mode.length,
				this.params.ifBw,
				this.params.pitch,
			);
		} finally {
			this.module.free(ptr, mode.length);
		}
	}

	/** Turn the audio-enhancement chain on/off (the RAW/digital comparison flips this). */
	setAudioEnabled(on: boolean): void {
		if (this.handle) this.module.exports.websa_dsp_demod_set_audio(this.handle, on ? 1 : 0);
	}

	/** Noise reduction on/off plus its strength (0..1): the panel's NR control. */
	setNr(on: boolean, strength: number): void {
		if (this.handle) {
			this.module.exports.websa_dsp_demod_set_nr(this.handle, on ? 1 : 0, strength);
		}
	}

	/** The squelch gate's threshold in dBFS (<= -100 leaves it wide open). */
	setSquelch(dbfs: number): void {
		if (this.handle) this.module.exports.websa_dsp_demod_set_squelch(this.handle, dbfs);
	}

	/** The panel's AGC switch: on = the level stage adapts (default), off = unity gain. */
	setAgc(on: boolean): void {
		if (this.handle) this.module.exports.websa_dsp_demod_set_agc?.(this.handle, on ? 1 : 0);
	}

	/**
	 * De-emphasis in microseconds. `< 0` is the panel's Auto: the DSP resolves it against its own
	 * mode table (50 us for broadcast WFM, none elsewhere), `0` switches it off, a positive value
	 * sets it. Mapping Auto to 0 here is what made the control look dead *and* left WFM running with
	 * its de-emphasis off.
	 */
	setDeemph(tauUs: number): void {
		if (!this.handle || this.digital) return;
		this.module.exports.websa_dsp_demod_set_deemph(
			this.handle,
			Number.isFinite(tauUs) ? tauUs : -1,
		);
	}

	/** The de-emphasis in force, in microseconds (0 = none), as the DSP resolved it. */
	deemphInForce(): number {
		if (!this.handle || this.digital) return 0;
		return this.module.exports.websa_dsp_demod_deemph(this.handle);
	}

	setVolume(volume: number): void {
		this.volume = Number.isFinite(volume) ? Math.max(0, Math.min(4, volume)) : 1;
	}

	/** The backend retuned: drop the demodulator's channel history, keep its level. */
	retune(): void {
		if (this.handle) this.module.exports.websa_dsp_demod_retune(this.handle);
	}

	/** The backend restarted its DDC on the same station. Keep WFM's pilot/RDS metadata. */
	retuneStationary(): void {
		if (!this.handle) return;
		const soft = this.module.exports.websa_dsp_demod_retune_stationary;
		if (soft) soft(this.handle);
		else this.retune();
	}

	/** Clear the streaming state (a baseband stream restart). */
	reset(): void {
		if (this.handle) this.module.exports.websa_dsp_demod_reset(this.handle);
	}

	/** How many messages a digital pipeline has decoded. */
	count(): number {
		return this.handle ? this.module.exports.websa_dsp_demod_count(this.handle) : 0;
	}

	/** Complex samples a digital decoder has buffered towards its next attempt. */
	buffered(): number {
		return this.handle ? this.module.exports.websa_dsp_demod_buffered(this.handle) : 0;
	}

	/** Equalised constellation points (I,Q) the digital decoder wants to show. */
	constellation(maxPoints: number): { re: number; im: number }[] {
		if (!this.handle || !this.digital || !this.constPtr) return [];
		const capacity = Math.min(maxPoints, 2048);
		const count = this.module.exports.websa_dsp_drm_constellation(this.handle, this.constPtr, capacity);
		if (count === 0) return [];
		const view = this.module.f64View(this.constPtr, count * 2);
		const out: { re: number; im: number }[] = new Array(count);
		for (let i = 0; i < count; i++) out[i] = { re: view[2 * i], im: view[2 * i + 1] };
		return out;
	}

	/** Decoded DRM audio PCM (float, −1..1) at the rate reported by `audioRate()`. */
	audioPcm(maxSamples: number): Float32Array {
		if (!this.handle || !this.digital || !this.audioPtr) return new Float32Array(0);
		const count = this.module.exports.websa_dsp_drm_audio_pcm(
			this.handle,
			this.audioPtr,
			Math.min(maxSamples, 32768),
		);
		if (count === 0) return new Float32Array(0);
		const view = this.module.i16View(this.audioPtr, count);
		const out = new Float32Array(count);
		for (let i = 0; i < count; i++) out[i] = view[i] / 32768;
		return out;
	}

	/** The decoded DRM audio PCM's sample rate in Hz (0 when there is no audio). */
	audioRate(): number {
		return this.handle && this.digital ? this.module.exports.websa_dsp_drm_audio_rate(this.handle) : 0;
	}

	/**
	 * Drain the stereo difference signal (L−R) of the last processed block, at the consumer's
	 * rate, aligned one-to-one with the mono PCM of `process()`. Empty for the digital path and
	 * for the modes without a side chain (a baseband too narrow for the subcarriers).
	 */
	side(): Float32Array {
		if (!this.handle || this.digital) return new Float32Array(0);
		const written = this.module.exports.websa_dsp_demod_side(this.handle, this.sidePtr, MAX_COMPLEX_SAMPLES);
		if (written <= 0) return new Float32Array(0);
		const view = this.module.f32View(this.sidePtr, written);
		const side = new Float32Array(view); // the next block overwrites the block
		if (this.volume !== 1) {
			for (let index = 0; index < written; index++) side[index] *= this.volume;
		}
		return side;
	}

	/** Whether the stereo pilot is currently detected (the badge's state). */
	stereo(): boolean {
		return this.handle !== 0
			&& this.module.exports.websa_dsp_demod_stereo(this.handle) === 1;
	}

	/** FM stereo and RDS state, or null when no side chain: `[stereo, peak_dev_hz, pilot_ratio,
	 * rds_locked, rds_pi, rds_groups, pilot_dev_hz, pilot_snr_db]`. */
	stereoInfo(): {
		stereo: boolean; peakDevHz: number; pilotRatio: number;
		rdsLocked: boolean; rdsPi: number; rdsGroups: number; pilotDevHz: number; pilotSnrDb: number;
		/** Bench diagnostics, past the product's own eight words (zero when absent). */
		pilotI: number; pilotQ: number; auxRateHz: number; lastAux: number;
		rdsRefMag: number; rdsAxisI: number; rdsAxisQ: number; rdsBits: number;
	} | null {
		if (!this.handle || this.digital) return null;
		// 16 f64 words (8 product + 8 bench diagnostics); the ABI writes all sixteen, so the
		// block must cover them — a short block corrupts the wasm heap.
		const ptr = this.module.alloc(16 * 8);
		if (!ptr) return null;
		try {
			const words = this.module.exports.websa_dsp_demod_stereo_info(this.handle, ptr);
			if (words < 8) return null;
			// A fresh view: the call may have grown (and detached) the memory.
			const v = new Float64Array(this.module.exports.memory.buffer, ptr, words);
			return {
				stereo: v[0] !== 0, peakDevHz: v[1], pilotRatio: v[2],
				rdsLocked: v[3] !== 0, rdsPi: v[4], rdsGroups: v[5], pilotDevHz: v[6],
				pilotSnrDb: v[7],
				pilotI: words >= 12 ? v[8] : 0, pilotQ: words >= 12 ? v[9] : 0,
				auxRateHz: words >= 12 ? v[10] : 0, lastAux: words >= 12 ? v[11] : 0,
				rdsRefMag: words >= 16 ? v[12] : 0, rdsAxisI: words >= 16 ? v[13] : 0,
				rdsAxisQ: words >= 16 ? v[14] : 0, rdsBits: words >= 16 ? v[15] : 0,
			};
		} finally {
			this.module.free(ptr, 16 * 8);
		}
	}

	/** The decoded RDS text: the eight-character programme service name and the radio text, or
	 * null when the side chain has decoded nothing yet. */
	rdsText(): { ps: string; rt: string } | null {
		if (!this.handle || this.digital) return null;
		const ptr = this.module.alloc(18 * 4);
		if (!ptr) return null;
		try {
			const words = this.module.exports.websa_dsp_demod_rds_text(this.handle, ptr);
			if (words !== 18) return null;
			const v = new Uint32Array(this.module.exports.memory.buffer, ptr, 18);
			const chars = (word: number): string[] => [0, 8, 16, 24].map((shift) =>
				String.fromCharCode((word >>> shift) & 0xFF));
			let rt = '';
			for (let k = 2; k < 18; k++) rt += chars(v[k]).join('');
			return {
				ps: [...chars(v[0]), ...chars(v[1])].join(''),
				rt,
			};
		} finally {
			this.module.free(ptr, 18 * 4);
		}
	}

	/** Rebuild for a new mode/geometry. The IO blocks stay: only the handle is replaced (freeing the
	 * blocks here would leave the next `process` writing to a null pointer, which reads as a silent
	 * zero-length block). */
	reconfigure(params: PipelineParams, digital: boolean): void {
		// Idempotent: a caller that re-sends the configuration it already has (a STATUS-driven sync,
		// once a second) must not cost the chain its state. Rebuilding resets the filters and the
		// level stage, and the first blocks after that are silence - a dropout the listener hears as a
		// periodic pulse on a quiet band.
		if (this.handle && this.digital === digital && this.params
			&& this.params.mode === params.mode
			&& this.params.ifBw === params.ifBw
			&& this.params.pitch === params.pitch
			&& this.params.outRate === params.outRate
			&& this.params.deemphUs === params.deemphUs
			&& Math.abs(this.params.fsIn - params.fsIn) <= Math.max(200, params.fsIn * 0.01)) {
			return;
		}
		if (this.handle) {
			this.module.exports.websa_dsp_demod_free(this.handle);
			this.handle = 0;
		}
		this.params = params;
		this.digital = digital;

		this.create();
	}

	private writeInput(baseband: Float32Array): number {
		const complex = Math.min(baseband.length >> 1, MAX_COMPLEX_SAMPLES);
		if (complex === 0) return 0;
		// Views after the allocations in the constructor, never before: `alloc` may grow the memory.
		this.module.f32View(this.inPtr, complex * 2).set(baseband.subarray(0, complex * 2));
		return complex;
	}

	/** Run one baseband block through the analog path; returns the PCM (empty when unusable). */
	process(baseband: Float32Array, hold = false): Float32Array<ArrayBuffer> {
		if (!this.handle || this.digital || baseband.length === 0) return new Float32Array(0);
		const complex = this.writeInput(baseband);
		if (complex === 0) return new Float32Array(0);
		const written = this.module.exports.websa_dsp_demod_process(
			this.handle,
			this.inPtr,
			complex,
			this.outPtr,
			MAX_COMPLEX_SAMPLES,
			hold ? 1 : 0,
		);
		if (written <= 0) return new Float32Array(0);
		const view = this.module.f32View(this.outPtr, written);
		// A copy, because the next call overwrites the block. Volume is a listener preference, the
		// last thing before the speaker, so it is applied here rather than in the DSP.
		const pcm = new Float32Array(written);
		if (this.volume === 1) {
			pcm.set(view);
		} else {
			for (let index = 0; index < written; index++) pcm[index] = view[index] * this.volume;
		}
		return pcm;
	}

	/** Feed one baseband block to the digital path; returns every message decoded by it. */
	push(baseband: Float32Array): Ft8Report[] {
		if (!this.handle || !this.digital || baseband.length === 0) return [];
		const complex = this.writeInput(baseband);
		if (complex === 0) return [];
		const count = this.module.exports.websa_dsp_demod_push(this.handle, this.inPtr, complex);
		if (count === 0) return [];
		const reports: Ft8Report[] = [];
		const total = this.count();
		for (let index = 0; index < count; index++) {
			const metrics = this.module.f64View(this.metricsPtr, 3);
			const length = this.module.exports.websa_dsp_demod_message_at(
				this.handle,
				index,
				this.textPtr,
				TEXT_CAPACITY,
				this.metricsPtr,
			);
			if (length === 0) continue;
			const text = new TextDecoder().decode(this.module.u8View(this.textPtr, length));
			reports.push({
				text,
				frequencyHz: metrics[0],
				timeOffsetS: metrics[1],
				snrDb: metrics[2],
				count: total,
				// The decoder measures inside its channel; the caller knows which channel that was (it
				// is the baseband frame's centre) and fills this in.
				centerHz: 0,
			});
		}
		return reports;
	}

	/**
	 * The decoded picture's state, when the protocol produces one (SSTV). `null` before anything
	 * decoded. Read before {@link image}: the pane sizes its canvas from the geometry and shows the
	 * readout even while the picture is still arriving.
	 */
	imageStatus(): SstvImageStatus | null {
		if (!this.handle || !this.imgMetaPtr) return null;
		const words = this.module.exports.websa_dsp_demod_image_info(this.handle, this.imgMetaPtr);
		if (words !== IMAGE_WORDS) return null;
		const meta = new Uint32Array(this.module.exports.memory.buffer, this.imgMetaPtr, IMAGE_WORDS);
		return {
			width: meta[0],
			height: meta[1],
			lines: meta[2],
			progress: meta[3],
			vis: meta[4],
			serial: meta[5] + meta[6] * 0x1_0000_0000,
			complete: meta[7] === 1,
			modeIndex: meta[8],
		};
	}

	/**
	 * The decoded picture's pixels, copied out of the module's memory.
	 *
	 * A copy on every update rather than a view: the demodulator keeps the picture until a newer one
	 * replaces it, and this buffer is what the worker transfers to the page.
	 */
	image(): SstvPicture | null {
		if (!this.handle || !this.imgPtr) return null;
		const status = this.imageStatus();
		if (!status) return null;
		const bytes = status.width * status.height * 3;
		if (bytes === 0 || bytes > IMAGE_CAPACITY) return null;
		const written = this.module.exports.websa_dsp_demod_image_data(this.handle, this.imgPtr, IMAGE_CAPACITY);
		if (written !== bytes) return null;
		return {
			rgb: new Uint8Array(this.module.u8View(this.imgPtr, bytes)),
			width: status.width,
			height: status.height,
		};
	}

	/** Pin the decoder to one of its own modes (`null` = the signal decides). True when it took it. */
	setModePin(index: number | null): boolean {
		if (!this.handle) return false;
		return this.module.exports.websa_dsp_demod_set_mode_pin(this.handle, index === null ? -1 : index) === 1;
	}

	/**
	 * What the decoder is doing while nothing has decoded: attempts made, audio buffered, and whether a
	 * picture exists at all. `null` for a protocol that reports no state.
	 */
	decodeState(): SstvDecoderCounters | null {
		if (!this.handle || !this.imgMetaPtr) return null;
		const words = this.module.exports.websa_dsp_demod_decode_state(this.handle, this.imgMetaPtr);
		if (words !== DECODE_STATE_WORDS) return null;
		const meta = new Uint32Array(this.module.exports.memory.buffer, this.imgMetaPtr, DECODE_STATE_WORDS);
		const bufferedSamples = meta[2];
		return {
			attempts: meta[0] + meta[1] * 0x1_0000_0000,
			bufferedSamples,
			bufferedSeconds: bufferedSamples / SSTV_DECODE_RATE_HZ,
			hasPicture: meta[3] === 1,
			level: meta[4] / 1000,
		};
	}

	free(): void {
		if (this.handle) {
			this.module.exports.websa_dsp_demod_free(this.handle);
			this.handle = 0;
		}
		if (this.inPtr) {
			this.module.free(this.inPtr, MAX_COMPLEX_SAMPLES * 2 * 4);
			this.inPtr = 0;
		}
		if (this.outPtr) {
			this.module.free(this.outPtr, MAX_COMPLEX_SAMPLES * 4);
			this.outPtr = 0;
		}
		if (this.textPtr) {
			this.module.free(this.textPtr, TEXT_CAPACITY);
			this.textPtr = 0;
		}
		if (this.metricsPtr) {
			this.module.free(this.metricsPtr, 3 * 8);
			this.metricsPtr = 0;
		}
		if (this.constPtr) {
			this.module.free(this.constPtr, 2048 * 2 * 8);
			this.constPtr = 0;
		}
		if (this.audioPtr) {
			this.module.free(this.audioPtr, 32768 * 2);
			this.audioPtr = 0;
		}
		if (this.sidePtr) {
			this.module.free(this.sidePtr, MAX_COMPLEX_SAMPLES * 4);
			this.sidePtr = 0;
		}
		if (this.imgMetaPtr) {
			this.module.free(this.imgMetaPtr, IMAGE_WORDS * 4);
			this.imgMetaPtr = 0;
		}
		if (this.imgPtr) {
			this.module.free(this.imgPtr, IMAGE_CAPACITY);
			this.imgPtr = 0;
		}
	}
}
