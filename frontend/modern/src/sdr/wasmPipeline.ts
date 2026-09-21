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
import type { Ft8Report, PipelineParams } from './types';

/** Blocks larger than this are truncated rather than reallocating in the audio path. */
const MAX_COMPLEX_SAMPLES = 32_768;
const TEXT_CAPACITY = 64;

export type { Ft8Report, PipelineParams };

export class WasmPipeline {
	private inPtr = 0;
	private outPtr = 0;
	private textPtr = 0;
	private metricsPtr = 0;
	private handle = 0;
	private volume = 1;

	constructor(private module: DspModule, private params: PipelineParams, private digital: boolean) {
		this.inPtr = module.alloc(MAX_COMPLEX_SAMPLES * 2 * 4);
		this.outPtr = module.alloc(MAX_COMPLEX_SAMPLES * 4);
		this.textPtr = module.alloc(TEXT_CAPACITY);
		this.metricsPtr = module.alloc(3 * 8);
		if (!this.inPtr || !this.outPtr || !this.textPtr || !this.metricsPtr) return;
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

	/** Feed one baseband block to the digital path; returns the decoded message when there is one. */
	push(baseband: Float32Array): Ft8Report | null {
		if (!this.handle || !this.digital || baseband.length === 0) return null;
		const complex = this.writeInput(baseband);
		if (complex === 0) return null;
		const decoded = this.module.exports.websa_dsp_demod_push(this.handle, this.inPtr, complex);
		if (decoded !== 1) return null;
		const metrics = this.module.f64View(this.metricsPtr, 3);
		const length = this.module.exports.websa_dsp_demod_message(
			this.handle,
			this.textPtr,
			TEXT_CAPACITY,
			this.metricsPtr,
		);
		if (length === 0) return null;
		const text = new TextDecoder().decode(this.module.u8View(this.textPtr, length));
		return {
			text,
			frequencyHz: metrics[0],
			timeOffsetS: metrics[1],
			snrDb: metrics[2],
			count: this.count(),
			// The decoder measures inside its channel; the caller knows which channel that was (it
			// is the baseband frame's centre) and fills this in.
			centerHz: 0,
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
	}
}
