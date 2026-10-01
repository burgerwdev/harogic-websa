// DeepFilterNet3 noise reduction via the upstream `libDF`/tract WASM runtime.
//
// This replaces the earlier JS port + onnxruntime-web approach. The whole pipeline (STFT, ERB,
// encoder/decoders, deep filter, ISTFT) runs inside the author's own Rust WASM, one
// `df_process_frame` per 480-sample hop, with the conv lookahead and the GRU hidden state handled
// correctly by tract's PulsedModel + SimpleState. The model weights are fetched at runtime and
// handed to `df_create` as the tar.gz bytes — they are not embedded in the WASM.
//
// `df_process_frame` is synchronous, so the worker no longer needs the async serialization chain
// the onnxruntime-web path required.

export interface DfnUrls {
	/** The wasm-bindgen glue module (ESM). */
	js: string;
	/** `df_bg.wasm`, fetched by the glue. */
	wasm: string;
	/** `DeepFilterNet3_onnx.tar.gz`, passed to `df_create`. */
	model: string;
}

/** Asset URLs under Vite's base. */
export function dfnUrls(base?: string): DfnUrls {
	const root = new URL(
		base ?? (import.meta as { env?: { BASE_URL?: string } }).env?.BASE_URL ?? '/static/dist/',
		globalThis.location ? location.origin : 'http://localhost',
	).href;
	return {
		js: new URL('dfn/df.js', root).href,
		wasm: new URL('dfn/df_bg.wasm', root).href,
		model: new URL('models/dfn/DeepFilterNet3_onnx.tar.gz', root).href,
	};
}

export interface DfnRuntime {
	/** Create a stream state from the model tar.gz bytes; returns an opaque pointer. */
	create(model: Uint8Array, attenLimDb: number): number;
	/** Process one hop (480 samples) in place of the whole pipeline; returns 480 samples. */
	processFrame(ptr: number, input: Float32Array): Float32Array;
	/** Hop size in samples (480). */
	frameLength(): number;
	setAttenLim(ptr: number, db: number): void;
}

/** Load the glue, instantiate the WASM, and return the runtime API. */
export async function loadDfn(urls: DfnUrls): Promise<DfnRuntime> {
	const mod: any = await import(/* @vite-ignore */ urls.js);
	await mod.default(urls.wasm);
	return {
		create: (model, attenLimDb) => mod.df_create(model, attenLimDb) as number,
		processFrame: (ptr, input) => mod.df_process_frame(ptr, input) as Float32Array,
		frameLength: () => 480,
		setAttenLim: (ptr, db) => mod.df_set_atten_lim(ptr, db),
	};
}

/**
 * Streams arbitrary-length 48 kHz PCM through the tract runtime, buffering into 480-sample hops.
 * Output is 1:1 with the input (delayed by the model's lookahead) and never zero-padded.
 */
export class DfnProcessor {
	private readonly runtime: DfnRuntime;
	private readonly model: Uint8Array;
	private attenLimDb: number;
	private ptr: number;
	private inBuf = new Float32Array(0);
	private outQueue: Float32Array[] = [];

	constructor(runtime: DfnRuntime, model: Uint8Array, attenLimDb = 6) {
		this.runtime = runtime;
		this.model = model;
		this.attenLimDb = attenLimDb;
		this.ptr = runtime.create(model, attenLimDb);
	}

	reset(): void {
		// The tract stream has no separate reset; recreating the state drops the lookahead history
		// and the GRU state (what a retune needs). The model bytes are already in memory, so this
		// re-parses them rather than re-fetching.
		this.ptr = this.runtime.create(this.model, this.attenLimDb);
		this.inBuf = new Float32Array(0);
		this.outQueue = [];
	}

	/** Change the attenuation limit live (the tract runtime applies it without a rebuild). */
	setAttenLim(db: number): void {
		this.attenLimDb = db;
		this.runtime.setAttenLim(this.ptr, db);
	}

	private queued(): number {
		let n = 0;
		for (const block of this.outQueue) n += block.length;
		return n;
	}

	process(input: Float32Array): Float32Array {
		const hop = this.runtime.frameLength();
		const combined = new Float32Array(this.inBuf.length + input.length);
		combined.set(this.inBuf, 0);
		combined.set(input, this.inBuf.length);
		let offset = 0;
		while (offset + hop <= combined.length) {
			// Copy: the hop view must not alias `combined` across the (synchronous) wasm call.
			this.outQueue.push(this.runtime.processFrame(this.ptr, combined.subarray(offset, offset + hop)));
			offset += hop;
		}
		this.inBuf = combined.slice(offset);

		const out = new Float32Array(Math.min(input.length, this.queued()));
		let outIdx = 0;
		while (outIdx < out.length && this.outQueue.length > 0) {
			const block = this.outQueue[0];
			const take = Math.min(block.length, out.length - outIdx);
			out.set(block.subarray(0, take), outIdx);
			outIdx += take;
			if (take === block.length) this.outQueue.shift();
			else this.outQueue[0] = block.subarray(take);
		}
		return out;
	}
}
