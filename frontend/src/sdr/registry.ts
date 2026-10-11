// The SDR plugin registry, read from the DSP module itself.
//
// The browser must not own a second list of modes. The Rust plugin registry is the single source
// of truth and is exported over the ABI (`websa_dsp_plugin_*`), so this module *reads* it: a mode
// that exists in the DSP but not in the UI is impossible, and a UI entry without a kernel cannot
// be invented here. The mode dropdown is built from what this module returns.
//
// `implemented: false` means "declared, kernel not written yet": the UI lists it but must disable
// it, which is honest about what a build can actually do.

import { loadDsp, type DspModule } from './wasm';

export type PluginKind = 'analog' | 'digital' | 'audio' | 'ddc';

/** Order matters: it is the ABI's kind numbering. */
export const PLUGIN_KINDS: readonly PluginKind[] = ['analog', 'digital', 'audio', 'ddc'] as const;

export interface PluginInfo {
	kind: PluginKind;
	id: string;
	/** False while the kernel is not written yet (the UI disables such a mode). */
	implemented: boolean;
	/** True for audio-enhancement stages: analog PCM path only. */
	audioEnhancement: boolean;
}

function readPluginId(module: DspModule, kind: number, index: number): string {
	const length = module.exports.websa_dsp_plugin_id_len(kind, index);
	if (length <= 0) return '';
	const ptr = module.alloc(length);
	if (!ptr) return '';
	try {
		const written = module.exports.websa_dsp_plugin_id(kind, index, ptr, length);
		// A fresh view: allocating may have grown (and detached) the memory.
		const bytes = new Uint8Array(module.exports.memory.buffer, ptr, written);
		return new TextDecoder().decode(bytes);
	} finally {
		module.free(ptr, length);
	}
}

/** Read the whole plugin manifest out of an instantiated module. */
export function readPluginManifest(module: DspModule): PluginInfo[] {
	const plugins: PluginInfo[] = [];
	for (let kind = 0; kind < module.exports.websa_dsp_plugin_kind_count(); kind++) {
		const count = module.exports.websa_dsp_plugin_count(kind);
		for (let index = 0; index < count; index++) {
			const id = readPluginId(module, kind, index);
			if (!id) continue;
			plugins.push({
				kind: PLUGIN_KINDS[kind],
				id,
				implemented: module.exports.websa_dsp_plugin_implemented(kind, index) === 1,
				audioEnhancement: module.exports.websa_dsp_plugin_audio_enhancement(kind, index) === 1,
			});
		}
	}
	return plugins;
}

let manifest: PluginInfo[] | null = null;

/** One SSTV mode, as the Rust table declares it. The decode window's mode buttons come from here. */
export interface SstvModeInfo {
	/** Index in the table: what the pin setter takes. */
	index: number;
	name: string;
	/** The VIS code the calibration header carries. */
	vis: number;
	width: number;
	height: number;
}

let sstvModes: SstvModeInfo[] | null = null;

function readSstvModeName(module: DspModule, index: number): string {
	const length = module.exports.websa_dsp_sstv_mode_name_len(index);
	if (length <= 0) return '';
	const ptr = module.alloc(length);
	if (!ptr) return '';
	try {
		const written = module.exports.websa_dsp_sstv_mode_name(index, ptr, length);
		// A fresh view: allocating may have grown (and detached) the memory.
		return new TextDecoder().decode(new Uint8Array(module.exports.memory.buffer, ptr, written));
	} finally {
		module.free(ptr, length);
	}
}

/** Read the SSTV mode table out of an instantiated module. */
export function readSstvModeTable(module: DspModule): SstvModeInfo[] {
	const modes: SstvModeInfo[] = [];
	const count = module.exports.websa_dsp_sstv_mode_count();
	for (let index = 0; index < count; index++) {
		const name = readSstvModeName(module, index);
		if (!name) continue;
		const ptr = module.alloc(3 * 4);
		if (!ptr) continue;
		let vis = 0;
		let width = 0;
		let height = 0;
		let words = 0;
		try {
			words = module.exports.websa_dsp_sstv_mode_meta(index, ptr);
			if (words === 3) {
				const meta = new Uint32Array(module.exports.memory.buffer, ptr, 3);
				[vis, width, height] = [meta[0], meta[1], meta[2]];
			}
		} finally {
			module.free(ptr, 3 * 4);
		}
		if (words !== 3) continue;
		modes.push({ index, name, vis, width, height });
	}
	return modes;
}

/** Load (once) and cache the manifest and the SSTV mode table from the artifact at `url`. */
export async function loadPluginManifest(url: string): Promise<PluginInfo[]> {
	if (manifest) return manifest;
	const module = await loadDsp(url);
	manifest = readPluginManifest(module);
	sstvModes = readSstvModeTable(module);
	return manifest;
}

/** The cached manifest, or null before it has been loaded. */
export function pluginManifest(): PluginInfo[] | null {
	return manifest;
}

/**
 * The SSTV mode table (names, VIS codes, resolutions), or null before the module has been loaded.
 *
 * The decode window builds its mode buttons from this list, so a button cannot offer a mode the
 * decoder does not know — the same rule the mode dropdown follows for the demodulator list.
 */
export function sstvModeTable(): SstvModeInfo[] | null {
	return sstvModes;
}

function idsOf(kind: PluginKind): string[] {
	return (manifest ?? []).filter((plugin) => plugin.kind === kind).map((plugin) => plugin.id);
}

/** Analog demodulator ids, in registry order (the mode dropdown's source). */
export function analogModeIds(): string[] {
	return idsOf('analog');
}

/** Digital demodulator ids (FT8 first). */
export function digitalModeIds(): string[] {
	return idsOf('digital');
}

/**
 * True when `id` is a protocol decoder rather than a demodulator.
 *
 * The difference reaches the display: a decoder reads a fixed band of its own (FT8: 100..3000 Hz
 * above the dial) instead of an IF passband centred on it, so what the panadapter highlights has to
 * follow the kind. False before the manifest has loaded, which is safe: a digital mode cannot be
 * selected before then (its button comes from the same manifest).
 */
export function isDigitalMode(id: string): boolean {
	return digitalModeIds().includes(id);
}

/** Audio-enhancement stage ids, in chain order (analog PCM path only). */
export function audioStageIds(): string[] {
	return idsOf('audio');
}

/** DDC base-layer stage ids (shared by both paths). */
export function ddcStageIds(): string[] {
	return idsOf('ddc');
}

/**
 * Every mode the SDR panel offers: the analog demodulators and the digital protocols, in registry
 * order. One list for the button group and the keyboard cycle, so a mode cannot exist in one and be
 * missing from the other (the earlier hardcoded cycle in the panel carried `fm`, which is not a
 * plugin id at all and made the DSP report "no pipeline for mode fm").
 */
export function sdrModeIds(): string[] {
	return [...analogModeIds(), ...digitalModeIds()];
}

/**
 * The analog demodulator that plays a digital protocol's audio, or null when the mode has its own.
 *
 * A protocol decoder produces text and no audio, but the operator still wants to *hear* the channel
 * (FT8 is USB in practice). The two run side by side over the same baseband: the decoder reads it
 * unmodified, and the companion demodulator produces PCM through the audio chain - the pipeline's
 * separation rule keeps the chain away from the decoder's samples, so the two cannot interfere.
 */
export function audioCompanionFor(id: string): string | null {
	const plugin = (manifest ?? []).find((entry) => entry.id === id);
	if (!plugin || plugin.kind !== 'digital') return null;
	// DRM audio is decoded, not an analog sideband.
	if (id === 'drm' || id === 'drmplus') return null;
	return 'usb';
}

/** True when the mode is declared and its kernel exists. */
export function isModeAvailable(id: string): boolean {
	return (manifest ?? []).some((plugin) => plugin.id === id && plugin.implemented);
}

/** Test seam: replace the cached manifest (unit tests) or clear it (null). */
export function setPluginManifest(next: PluginInfo[] | null): void {
	manifest = next;
}

/** Test seam: replace the cached SSTV mode table (unit tests) or clear it (null). */
export function setSstvModeTable(next: SstvModeInfo[] | null): void {
	sstvModes = next;
}
