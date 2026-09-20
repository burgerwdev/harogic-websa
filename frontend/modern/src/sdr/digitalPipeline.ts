// The WASM *digital* pipeline, wrapped for the worker.
//
// Same shape as `WasmPipeline`, one layer up: the Rust side runs the DDC and then the protocol
// decoder (FT8), so the worker only moves IQ in and decoded text out. The DDC being shared is the
// point — the digital path differs from the analog one above the DDC, not below it — and it is why
// this wrapper exists instead of pushing raw IQ at a decoder that expects channelized baseband.
import type { DspModule } from './wasm';
import type { Ft8Report } from './iqStream';

export interface DigitalParams {
	fsIn: number;
	offsetHz: number;
	decimate: number;
	outRate: number;
	/** Digital plugin id (ft8). */
	mode: string;
}

const MAX_COMPLEX_SAMPLES = 32_768;
const TEXT_CAPACITY = 64;

export class DigitalPipeline {
	private handle = 0;
	private inPtr = 0;
	private textPtr = 0;
	private metricsPtr = 0;

	constructor(private module: DspModule, private params: DigitalParams) {
		this.inPtr = module.alloc(MAX_COMPLEX_SAMPLES * 2 * 2);   // int16 I/Q
		this.textPtr = module.alloc(TEXT_CAPACITY);
		this.metricsPtr = module.alloc(3 * 8);
		if (!this.inPtr || !this.textPtr || !this.metricsPtr) return;
		module.u8View(this.textPtr, TEXT_CAPACITY).fill(0);
		this.handle = this.create(this.params);
	}

	private create(params: DigitalParams): number {
		const mode = new TextEncoder().encode(params.mode);
		const ptr = this.module.alloc(mode.length);
		if (!ptr) return 0;
		try {
			this.module.u8View(ptr, mode.length).set(mode);
			return this.module.exports.websa_dsp_digital_new(
				params.fsIn,
				params.offsetHz,
				params.decimate,
				params.outRate,
				ptr,
				mode.length,
			);
		} finally {
			this.module.free(ptr, mode.length);
		}
	}

	get ok(): boolean {
		return this.handle !== 0;
	}

	get mode(): string {
		return this.params.mode;
	}

	/** How many messages this pipeline has decoded. */
	count(): number {
		return this.handle ? this.module.exports.websa_dsp_digital_count(this.handle) : 0;
	}

	/** Complex samples buffered towards the next decode attempt (0 when unusable). */
	buffered(): number {
		return this.handle ? this.module.exports.websa_dsp_digital_buffered(this.handle) : 0;
	}

	/** Feed one IQ block; returns the decoded message when this block completed one. */
	push(iq: Int16Array): Ft8Report | null {
		if (!this.handle || iq.length === 0) return null;
		const complex = Math.min(iq.length >> 1, MAX_COMPLEX_SAMPLES);
		if (complex === 0) return null;
		this.module.i16View(this.inPtr, complex * 2).set(iq.subarray(0, complex * 2));
		const decoded = this.module.exports.websa_dsp_digital_push(this.handle, this.inPtr, complex);
		if (decoded !== 1) return null;
		const metrics = this.module.f64View(this.metricsPtr, 3);
		const length = this.module.exports.websa_dsp_digital_message(
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
		};
	}

	/** Rebuild for a new geometry (the IO blocks stay). */
	reconfigure(params: DigitalParams): void {
		if (this.handle) {
			this.module.exports.websa_dsp_digital_free(this.handle);
			this.handle = 0;
		}
		this.params = params;
		this.handle = this.create(params);
	}

	reset(): void {
		if (this.handle) this.module.exports.websa_dsp_digital_reset(this.handle);
	}

	free(): void {
		if (this.handle) {
			this.module.exports.websa_dsp_digital_free(this.handle);
			this.handle = 0;
		}
		if (this.inPtr) {
			this.module.free(this.inPtr, MAX_COMPLEX_SAMPLES * 2 * 2);
			this.inPtr = 0;
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
