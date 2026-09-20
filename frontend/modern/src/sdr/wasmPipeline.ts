// The WASM receive pipeline, wrapped for the worker.
//
// One handle per configured demodulator; one call per IQ block. The Rust side owns the
// orchestration (DDC -> analog demod -> audio chain), so nothing here knows the order of stages —
// it allocates the two blocks, marshals the samples and reads the PCM back. That is the whole
// point of the raw-pointer ABI: no per-sample JavaScript work in the hot path.
//
// Views are created after every allocation (growing the memory detaches existing views, which
// reads as length 0 rather than throwing).
import type { DspModule } from './wasm';

export interface PipelineParams {
	fsIn: number;
	offsetHz: number;
	decimate: number;
	outRate: number;
	/** Analog plugin id (am/dsb/usb/lsb/cw/nfm/wfm/pm). */
	mode: string;
	ifBw: number;
	pitch: number;
}

/** Blocks larger than this are truncated rather than reallocating in the audio path. */
const MAX_COMPLEX_SAMPLES = 32_768;

export class WasmPipeline {
	private inPtr = 0;
	private outPtr = 0;
	private handle = 0;
	private volume = 1;

	constructor(private module: DspModule, private params: PipelineParams) {
		this.inPtr = module.alloc(MAX_COMPLEX_SAMPLES * 2 * 2);
		this.outPtr = module.alloc(MAX_COMPLEX_SAMPLES * 4);
		this.create();
	}

	/** True when the Rust side accepted the mode and geometry. */
	get ok(): boolean {
		return this.handle !== 0;
	}

	get mode(): string {
		return this.params.mode;
	}

	private create(): void {
		const mode = new TextEncoder().encode(this.params.mode);
		const ptr = this.module.alloc(mode.length);
		if (!ptr) return;
		try {
			this.module.u8View(ptr, mode.length).set(mode);
			this.handle = this.module.exports.websa_dsp_pipeline_new(
				this.params.fsIn,
				this.params.offsetHz,
				this.params.decimate,
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
		if (this.handle) this.module.exports.websa_dsp_pipeline_set_audio(this.handle, on ? 1 : 0);
	}

	setVolume(volume: number): void {
		this.volume = Number.isFinite(volume) ? Math.max(0, Math.min(4, volume)) : 1;
	}

	retune(offsetHz: number): void {
		this.params.offsetHz = offsetHz;
		if (this.handle) this.module.exports.websa_dsp_pipeline_retune(this.handle, offsetHz);
	}

	/** Rebuild for a new mode/geometry. The IO blocks stay: only the handle is replaced (freeing the
	 * blocks here would leave the next `process` writing to a null pointer, which reads as a silent
	 * zero-length block). */
	reconfigure(params: PipelineParams): void {
		if (this.handle) {
			this.module.exports.websa_dsp_pipeline_free(this.handle);
			this.handle = 0;
		}
		this.params = params;
		this.create();
	}

	/** Run one IQ block; returns the PCM produced (empty when the pipeline is unusable). */
	process(iq: Int16Array, hold = false): Float32Array<ArrayBuffer> {
		if (!this.handle || iq.length === 0) return new Float32Array(0);
		const complex = Math.min(iq.length >> 1, MAX_COMPLEX_SAMPLES);
		if (complex === 0) return new Float32Array(0);
		// Views after the allocations above, never before: `alloc` may grow the memory.
		this.module.i16View(this.inPtr, complex * 2).set(iq.subarray(0, complex * 2));
		const written = this.module.exports.websa_dsp_pipeline_process(
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

	free(): void {
		if (this.handle) {
			this.module.exports.websa_dsp_pipeline_free(this.handle);
			this.handle = 0;
		}
		if (this.inPtr) {
			this.module.free(this.inPtr, MAX_COMPLEX_SAMPLES * 2 * 2);
			this.inPtr = 0;
		}
		if (this.outPtr) {
			this.module.free(this.outPtr, MAX_COMPLEX_SAMPLES * 4);
			this.outPtr = 0;
		}
	}
}
