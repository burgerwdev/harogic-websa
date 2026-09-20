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

/** Load (once) and cache the manifest from the artifact at `url`. */
export async function loadPluginManifest(url: string): Promise<PluginInfo[]> {
	if (manifest) return manifest;
	manifest = readPluginManifest(await loadDsp(url));
	return manifest;
}

/** The cached manifest, or null before it has been loaded. */
export function pluginManifest(): PluginInfo[] | null {
	return manifest;
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

/** Audio-enhancement stage ids, in chain order (analog PCM path only). */
export function audioStageIds(): string[] {
	return idsOf('audio');
}

/** DDC base-layer stage ids (shared by both paths). */
export function ddcStageIds(): string[] {
	return idsOf('ddc');
}

/** True when the mode is declared and its kernel exists. */
export function isModeAvailable(id: string): boolean {
	return (manifest ?? []).some((plugin) => plugin.id === id && plugin.implemented);
}

/** Test seam: replace the cached manifest (unit tests) or clear it (null). */
export function setPluginManifest(next: PluginInfo[] | null): void {
	manifest = next;
}
