// Mode and SSTV metadata supplied by the backend's native plugin registry.
export type PluginKind = 'analog' | 'digital' | 'audio' | 'ddc';
export const PLUGIN_KINDS: readonly PluginKind[] = ['analog', 'digital', 'audio', 'ddc'];
export interface PluginInfo {
	kind: PluginKind;
	id: string;
	implemented: boolean;
	audioEnhancement: boolean;
}
export interface SstvModeInfo {
	index: number;
	name: string;
	vis: number;
	width: number;
	height: number;
}
let manifest: PluginInfo[] | null = null;
let sstvModes: SstvModeInfo[] | null = null;
let pending: Promise<PluginInfo[]> | null = null;

/** Load native receiver capabilities without loading a browser DSP module. */
export async function loadPluginManifest(url = '/api/sdr/plugins'): Promise<PluginInfo[]> {
	if (manifest) return manifest;
	if (pending) return pending;
	pending = (async () => {
		const response = await fetch(url);
		if (!response.ok) throw new Error(`Receiver metadata: HTTP ${response.status}`);
		const body: unknown = await response.json();
		if (!body || typeof body !== 'object' || !('plugins' in body) || !('sstv_modes' in body)
			|| !Array.isArray(body.plugins) || !Array.isArray(body.sstv_modes)) throw new Error('Invalid receiver metadata');
		const plugins: PluginInfo[] = body.plugins.map((value: unknown) => {
			if (!value || typeof value !== 'object' || !('kind' in value) || !('id' in value) || !('implemented' in value)) throw new Error('Invalid receiver plugin');
			const p = value;
			if (typeof p.kind !== 'string' || !PLUGIN_KINDS.includes(p.kind as PluginKind) || typeof p.id !== 'string' || !p.id || typeof p.implemented !== 'boolean') {
				throw new Error('Invalid receiver plugin');
			}
			return { kind: p.kind as PluginKind, id: p.id, implemented: p.implemented,
				audioEnhancement: 'audioEnhancement' in p && p.audioEnhancement === true };
		});
		const modes: SstvModeInfo[] = body.sstv_modes.map((value: unknown, i: number) => {
			if (!value || typeof value !== 'object' || !('index' in value) || !('name' in value)
				|| !('vis' in value) || !('width' in value) || !('height' in value)) throw new Error('Invalid receiver SSTV mode');
			const m = value;
			if (m.index !== i || typeof m.name !== 'string' || typeof m.vis !== 'number' || !Number.isInteger(m.vis)
				|| typeof m.width !== 'number' || !Number.isInteger(m.width) || m.width <= 0
				|| typeof m.height !== 'number' || !Number.isInteger(m.height) || m.height <= 0) {
				throw new Error('Invalid receiver SSTV mode');
			}
			return { index: m.index, name: m.name, vis: m.vis, width: m.width, height: m.height };
		});
		manifest = plugins;
		sstvModes = modes;
		return plugins;
	})();
	try { return await pending; } finally { pending = null; }
}
export function pluginManifest(): PluginInfo[] | null { return manifest; }
export function sstvModeTable(): SstvModeInfo[] | null { return sstvModes; }
function idsOf(kind: PluginKind): string[] { return (manifest ?? []).filter((p) => p.kind === kind).map((p) => p.id); }
export function analogModeIds(): string[] { return idsOf('analog'); }
export function digitalModeIds(): string[] { return idsOf('digital'); }
export function isDigitalMode(id: string): boolean { return digitalModeIds().includes(id); }
export function audioStageIds(): string[] { return idsOf('audio'); }
export function ddcStageIds(): string[] { return idsOf('ddc'); }
export function sdrModeIds(): string[] { return [...analogModeIds(), ...digitalModeIds()]; }
export function audioCompanionFor(id: string): string | null {
	return isDigitalMode(id) && id !== 'drm' && id !== 'drmplus' ? 'usb' : null;
}
export function isModeAvailable(id: string): boolean { return (manifest ?? []).some((p) => p.id === id && p.implemented); }
export function setPluginManifest(next: PluginInfo[] | null): void { manifest = next; }
export function setSstvModeTable(next: SstvModeInfo[] | null): void { sstvModes = next; }
