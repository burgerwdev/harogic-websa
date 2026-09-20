// FT8 over the WASM ABI: the digital path's decoder, driven block by block.
//
// The Rust side owns the protocol (tone detection, Costas sync, LDPC, CRC, message unpacking); this
// wrapper only moves samples in and text out. IQ arrives from the wire as interleaved int16 and the
// ABI works in f32 baseband, so a block is scaled once into a reused WASM buffer — no per-sample
// JavaScript objects, and no allocation per block.
//
// Slot timing comes back with the message (where in the buffer the transmission started, and on
// which frequency), which is what the UI shows: an FT8 decode without its timing is not much use to
// an operator watching a band.
import type { DspModule } from './wasm';

/** One decoded transmission. */
export interface Ft8Message {
	text: string;
	/** Audio frequency of tone 0 in Hz. */
	frequencyHz: number;
	/** Where the transmission started inside the pushed buffer, in seconds. */
	timeOffsetS: number;
	/** Sync-correlation SNR estimate in dB (a diagnostic, not a calibrated measurement). */
	snrDb: number;
}

const TEXT_CAPACITY = 64;

export class Ft8Session {
	private handle = 0;
	private inPtr = 0;
	private textPtr = 0;
	private metricsPtr = 0;
	private readonly blockCapacity: number;

	constructor(private module: DspModule, rate: number, blockComplexSamples = 16_384) {
		this.blockCapacity = blockComplexSamples;
		this.inPtr = module.alloc(blockComplexSamples * 2 * 4);
		this.textPtr = module.alloc(TEXT_CAPACITY);
		this.metricsPtr = module.alloc(3 * 8);
		if (!this.inPtr || !this.textPtr || !this.metricsPtr) return;
		// A view only after the allocations above: growing the memory detaches existing views.
		this.module.u8View(this.textPtr, TEXT_CAPACITY).fill(0);
		this.handle = module.exports.websa_dsp_ft8_new(rate);
	}

	get ok(): boolean {
		return this.handle !== 0;
	}

	/** Feed one block of interleaved int16 IQ; returns a message when the block completed one. */
	push(iq: Int16Array): Ft8Message | null {
		if (!this.handle || iq.length === 0) return null;
		const complex = Math.min(iq.length >> 1, this.blockCapacity);
		if (complex === 0) return null;
		const block = this.module.f32View(this.inPtr, complex * 2);
		for (let index = 0; index < complex * 2; index++) block[index] = iq[index] / 32768;
		const decoded = this.module.exports.websa_dsp_ft8_push(this.handle, this.inPtr, complex);
		if (decoded !== 1) return null;
		const metrics = this.module.f64View(this.metricsPtr, 3);
		const length = this.module.exports.websa_dsp_ft8_message(
			this.handle,
			this.textPtr,
			TEXT_CAPACITY,
			this.metricsPtr,
		);
		if (length === 0) return null;
		const bytes = this.module.u8View(this.textPtr, length);
		return {
			text: new TextDecoder().decode(bytes),
			frequencyHz: metrics[0],
			timeOffsetS: metrics[1],
			snrDb: metrics[2],
		};
	}

	/** Messages decoded since the session was created. */
	count(): number {
		return this.handle ? this.module.exports.websa_dsp_ft8_count(this.handle) : 0;
	}

	reset(): void {
		if (this.handle) this.module.exports.websa_dsp_ft8_reset(this.handle);
	}

	free(): void {
		if (this.handle) {
			this.module.exports.websa_dsp_ft8_free(this.handle);
			this.handle = 0;
		}
		if (this.inPtr) {
			this.module.free(this.inPtr, this.blockCapacity * 2 * 4);
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
