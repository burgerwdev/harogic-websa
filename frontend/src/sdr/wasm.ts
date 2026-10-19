// The WASM boundary: load the committed DSP module and expose typed views over its memory.
//
// One entry point family: `websa_dsp_demod_*`, the demodulator that turns the backend's channelized
// baseband into PCM (analog modes) or decoded text (digital protocols). The channelizer itself is
// not here — it runs on the backend, where the device's own DDC does the expensive part.
//
// No wasm-bindgen: the module exports its own memory and a handful of C-style functions, so the
// whole bridge is this file. The DSP works on sample blocks, which is why the ABI is a pointer
// plus a length — the caller allocates once per stream and the kernels process in place.
//
// One rule matters more than the rest: **a view must be created after the last allocation**.
// Growing the module's memory detaches every existing ArrayBuffer view, and a detach shows up as
// a length-0 view (silent zeros) rather than an exception. Views are therefore built on demand
// by `f32View`/`i16View`, never cached.

/** ABI version this loader understands; must equal `wasm/src/abi.rs::ABI_VERSION`. */
// Version 2 added SSTV picture data. Version 3 adds WFM stereo PCM and RDS metadata.
// `retune_stationary` is an optional additive v3 export.
export const DSP_ABI_VERSION = 3;

export interface DspExports {
	memory: WebAssembly.Memory;
	websa_dsp_version(): number;
	websa_dsp_alloc(bytes: number): number;
	websa_dsp_free(ptr: number, bytes: number): void;
	websa_dsp_f32_bytes(): number;
	websa_dsp_block_align(): number;
	// The demodulator: baseband in, PCM or decoded text out
	websa_dsp_demod_new(fsIn: number, outRate: number, modePtr: number, modeLen: number, ifBw: number, pitch: number): number;
	websa_dsp_demod_process(handle: number, iqPtr: number, samples: number, outPtr: number, capacity: number, audioHold: number): number;
	websa_dsp_demod_push(handle: number, iqPtr: number, samples: number): number;
	websa_dsp_demod_message_at(handle: number, index: number, textPtr: number, textCapacity: number, metricsPtr: number): number;
	websa_dsp_demod_buffered(handle: number): number;
	websa_dsp_drm_constellation(handle: number, ptr: number, capacity: number): number;
	websa_dsp_drm_audio_pcm(handle: number, ptr: number, capacity: number): number;
	websa_dsp_drm_audio_rate(handle: number): number;
	/** Decoded picture state: nine `u32` words written into `metaPtr`; returns the word count (0 = none). */
	websa_dsp_demod_image_info(handle: number, metaPtr: number): number;
	/** Copy the decoded picture's RGB888 pixels into `ptr`; returns the bytes written (0 = none/too small). */
	websa_dsp_demod_image_data(handle: number, ptr: number, capacity: number): number;
	/** Pin the decoder to one of its own modes (`index < 0` = auto); 1 when the protocol took it. */
	websa_dsp_demod_set_mode_pin(handle: number, index: number): number;
	/** Decoder state while nothing has decoded: four `u32` words (attempts lo/hi, buffered, has picture). */
	websa_dsp_demod_decode_state(handle: number, metaPtr: number): number;
	websa_dsp_demod_count(handle: number): number;
	websa_dsp_demod_set_audio(handle: number, enabled: number): number;
	websa_dsp_demod_set_nr(handle: number, enabled: number, strength: number): number;
	websa_dsp_demod_set_deemph(handle: number, tauUs: number): number;
	websa_dsp_demod_deemph(handle: number): number;
	websa_dsp_demod_set_squelch(handle: number, dbfs: number): number;
	/** The panel's AGC switch (1 = adapt, 0 = unity). Absent on builds older than the AGC switch. */
	websa_dsp_demod_set_agc?(handle: number, on: number): number;
	/** Drain the stereo difference signal (L−R) of the last processed block (WFM stereo only). */
	websa_dsp_demod_side(handle: number, ptr: number, capacity: number): number;
	/** 1 while the stereo pilot is detected (the badge's state). */
	websa_dsp_demod_stereo(handle: number): number;
	/** FM stereo and RDS state: eight f64 values [stereo, peak_dev_hz, pilot_ratio, rds_locked,
	 * rds_pi, rds_groups, pilot_dev_hz, pilot_snr_db]; 0 = no side chain. */
	websa_dsp_demod_stereo_info(handle: number, metaPtr: number): number;
	websa_dsp_demod_rds_text(handle: number, metaPtr: number): number;
	websa_dsp_demod_retune(handle: number): number;
	/** Same-frequency DDC restart; keep WFM station metadata. Absent on older ABI v3 builds. */
	websa_dsp_demod_retune_stationary?(handle: number): number;
	websa_dsp_demod_reset(handle: number): number;
	websa_dsp_demod_free(handle: number): number;
	// Codec smoke exports (FDK AAC / xHE-AAC), used by the DRM audio fixture test
	websa_dsp_fdk_decode_adts(ptr: number, len: number): number;
	websa_dsp_fdk_decode_drm(ptr: number, len: number): number;
	websa_dsp_fdk_decode_drm_frames(ptr: number, len: number, frameLen: number): number;
	websa_dsp_xaac_decode(ptr: number, len: number): number;
	// Plugin manifest: the UI's mode list is read from here, never hardcoded
	websa_dsp_plugin_kind_count(): number;
	websa_dsp_plugin_count(kind: number): number;
	websa_dsp_plugin_id_len(kind: number, index: number): number;
	websa_dsp_plugin_id(kind: number, index: number, buf: number, capacity: number): number;
	websa_dsp_plugin_implemented(kind: number, index: number): number;
	websa_dsp_plugin_audio_enhancement(kind: number, index: number): number;
	// SSTV mode table: the decode window's mode buttons are read from here, like the mode list above
	websa_dsp_sstv_mode_count(): number;
	websa_dsp_sstv_mode_name_len(index: number): number;
	websa_dsp_sstv_mode_name(index: number, buf: number, capacity: number): number;
	websa_dsp_sstv_mode_meta(index: number, metaPtr: number): number;
}

export interface DspModule {
	exports: DspExports;
	/** Allocate a zeroed block; 0 means the host refused (out of memory). */
	alloc(bytes: number): number;
	free(ptr: number, bytes: number): void;
	/** Float32 view at an ABI pointer. Create it *after* the allocation it reads. */
	f32View(ptr: number, length: number): Float32Array;
	/** Interleaved int16 view at an ABI pointer (raw IQ blocks). */
	i16View(ptr: number, length: number): Int16Array;
	/** Float64 view at an ABI pointer (the FT8 message's measurements). */
	f64View(ptr: number, length: number): Float64Array;
	/** Byte view at an ABI pointer (plugin ids and other strings). */
	u8View(ptr: number, length: number): Uint8Array;
}

function asExports(exports: WebAssembly.Exports): DspExports {
	const typed = exports as unknown as DspExports;
	if (typeof typed.websa_dsp_version !== 'function') {
		throw new Error('dsp.wasm does not export websa_dsp_version');
	}
	if (!(typed.memory instanceof WebAssembly.Memory)) {
		throw new Error('dsp.wasm does not export its memory');
	}
	return typed;
}

/**
 * Wrap an instantiated module. Exported separately from the fetch so the ABI contract can be
 * tested against the committed artifact in unit tests (no network, no service).
 */
export function wrapDsp(instance: WebAssembly.Instance): DspModule {
	const exports = asExports(instance.exports);
	const version = exports.websa_dsp_version();
	if (version !== DSP_ABI_VERSION) {
		// A stale committed artifact paired with a newer loader is exactly what the manifest
		// check cannot catch, so the module is rejected here as well.
		throw new Error(`dsp.wasm ABI ${version}, loader expects ${DSP_ABI_VERSION}`);
	}
	return {
		exports,
		alloc: (bytes: number) => exports.websa_dsp_alloc(bytes),
		free: (ptr: number, bytes: number) => exports.websa_dsp_free(ptr, bytes),
		f32View: (ptr: number, length: number) =>
			new Float32Array(exports.memory.buffer, ptr, length),
		i16View: (ptr: number, length: number) =>
			new Int16Array(exports.memory.buffer, ptr, length),
		f64View: (ptr: number, length: number) =>
			new Float64Array(exports.memory.buffer, ptr, length),
		u8View: (ptr: number, length: number) =>
			new Uint8Array(exports.memory.buffer, ptr, length),
	};
}

/** Instantiate the committed module from its bytes (the path used by tests and by `loadDsp`). */
export async function instantiateDsp(bytes: ArrayBuffer | Uint8Array): Promise<DspModule> {
	// The module imports nothing: the toolchain is `cargo build --target wasm32-unknown-unknown`
	// with no dependencies, so there is no glue the host must provide.
	const result = (await WebAssembly.instantiate(bytes, {})) as
		WebAssembly.Instance | WebAssembly.WebAssemblyInstantiatedSource;
	return wrapDsp(result instanceof WebAssembly.Instance ? result : result.instance);
}

let module_ : DspModule | null = null;
let loading: Promise<DspModule> | null = null;

/** The loaded module, or null before the first `loadDsp()` resolves. */
export function dsp(): DspModule | null {
	return module_;
}

/**
 * Fetch and instantiate `dsp.wasm` once per page/worker. A failure is reported to the caller,
 * which falls back to the Python DSP path rather than running without a DSP.
 */
export async function loadDsp(url: string): Promise<DspModule> {
	if (module_) return module_;
	if (!loading) {
		loading = (async () => {
			// `no-cache` revalidates against the server's validator instead of trusting the
			// heuristic freshness a browser applies to an asset without `Cache-Control`. The ABI
			// version in the URL only changes when the INTERFACE does, so a rebuilt kernel
			// otherwise sits behind a cached copy for hours: the page keeps running the previous
			// DSP while the file on disk is new, and the bench measures the wrong build.
			const response = await fetch(url, { cache: 'no-cache' });
			if (!response.ok) throw new Error(`cannot fetch ${url}: HTTP ${response.status}`);
			const module = await instantiateDsp(await response.arrayBuffer());
			module_ = module;
			return module;
		})();
		loading.catch(() => { loading = null; });   // a retry must be possible after a failure
	}
	return loading;
}

/** Where the artifact sits, derived from the app's static base.
 *
 * The page can be served from `/` (the backend's index route) while the assets live under
 * `/static/dist/`, so `document.baseURI` is *not* where `dsp.wasm` is: deriving the path
 * from the document produced a 404 against the served page. Vite's base is the one source of
 * truth for both the dev server and the built bundle (`vite.config.ts`), so it is used here and
 * `base` remains overridable for tests.
 */
export function dspWasmUrl(base?: string): string {
	// The ABI version is in the URL so a browser (or a Worker, whose fetch has its own cache entry)
	// cannot pair a loader with an artifact it does not understand: the modules are large and served
	// with a long lifetime, and a stale one fails as "the decoder is running but does nothing".
	const query = `?abi=${DSP_ABI_VERSION}`;
	if (base) return new URL(`dsp.wasm${query}`, base).href;
	const viteBase = (import.meta as { env?: { BASE_URL?: string } }).env?.BASE_URL;
	if (viteBase) return new URL(`dsp.wasm${query}`, new URL(viteBase, location.origin)).href;
	return new URL(`/static/dist/dsp.wasm${query}`, location.origin).href;
}
