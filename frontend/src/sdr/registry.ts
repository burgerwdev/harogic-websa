// Receiver and SSTV metadata supplied by the backend's plugin catalog.
export type PluginKind = 'analog' | 'digital' | 'audio' | 'ddc';
export type PluginOutput = 'audio' | 'text' | 'table' | 'image' | 'status';
export type PluginSideband = 'symmetric' | 'upper' | 'lower';
export const PLUGIN_KINDS: readonly PluginKind[] = ['analog', 'digital', 'audio', 'ddc'];
interface PluginBase {
	kind: PluginKind;
	id: string;
	implemented: boolean;
	audioEnhancement: boolean;
}
export interface ModeDescriptor extends PluginBase {
	kind: 'analog' | 'digital';
	label: string;
	category: string;
	description: string;
	aliases: string[];
	provider: string;
	startupTimeoutMs: number;
	available: boolean;
	unavailableReason: string | null;
	defaultBandwidthHz: number;
	minBandwidthHz: number;
	maxBandwidthHz: number;
	bandwidthPresetsHz: number[];
	bandwidthEditable: boolean;
	inputRateHz: number | null;
	minimumRateHz: number;
	audioCompanion: string | null;
	sideband: PluginSideband;
	outputs: PluginOutput[];
	common: boolean;
}
export type PluginInfo = ModeDescriptor | (PluginBase & { kind: 'audio' | 'ddc' });
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

function parsePlugin(value: unknown): PluginInfo {
	if (!value || typeof value !== 'object') throw new Error('Invalid receiver plugin');
	const p = value as Record<string, unknown>;
	if ((p.kind === 'audio' || p.kind === 'ddc') && typeof p.id === 'string' && p.id
		&& typeof p.implemented === 'boolean') return {
		kind: p.kind, id: p.id, implemented: p.implemented, audioEnhancement: p.audioEnhancement === true,
	};
	const strings = ['id', 'label', 'category', 'description', 'provider'] as const;
	const booleans = ['implemented', 'available', 'bandwidthEditable', 'common'] as const;
	const positive = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v) && v > 0;
	if (p.kind !== 'analog' && p.kind !== 'digital'
		|| strings.some((key) => typeof p[key] !== 'string') || !p.id || !p.label || !p.provider
		|| booleans.some((key) => typeof p[key] !== 'boolean')
		|| !Array.isArray(p.aliases) || !p.aliases.every((alias) => typeof alias === 'string')
		|| !Array.isArray(p.outputs) || !p.outputs.every((kind) => ['audio', 'text', 'table', 'image', 'status'].includes(kind))
		|| !['symmetric', 'upper', 'lower'].includes(p.sideband as string)
		|| !(p.unavailableReason === null || typeof p.unavailableReason === 'string')
		|| !(p.audioCompanion === null || typeof p.audioCompanion === 'string')
		|| !(p.inputRateHz === null || positive(p.inputRateHz))
		|| !positive(p.startupTimeoutMs) || !Number.isInteger(p.startupTimeoutMs) || p.startupTimeoutMs > 120000
		|| !positive(p.defaultBandwidthHz) || !positive(p.minBandwidthHz) || !positive(p.maxBandwidthHz)
		|| typeof p.minimumRateHz !== 'number' || !Number.isFinite(p.minimumRateHz) || p.minimumRateHz < 0
		|| p.minBandwidthHz > p.defaultBandwidthHz || p.defaultBandwidthHz > p.maxBandwidthHz
		|| !Array.isArray(p.bandwidthPresetsHz) || !p.bandwidthPresetsHz.every((hz) => positive(hz)
			&& hz >= (p.minBandwidthHz as number) && hz <= (p.maxBandwidthHz as number))
		|| (p.available && !p.implemented)) throw new Error('Invalid receiver plugin');
	return {
		kind: p.kind, id: p.id as string, implemented: p.implemented as boolean,
		audioEnhancement: p.audioEnhancement === true, label: p.label as string,
		category: p.category as string, description: p.description as string, aliases: [...p.aliases] as string[],
		provider: p.provider as string, available: p.available as boolean, unavailableReason: p.unavailableReason as string | null,
		startupTimeoutMs: p.startupTimeoutMs,
		defaultBandwidthHz: p.defaultBandwidthHz, minBandwidthHz: p.minBandwidthHz, maxBandwidthHz: p.maxBandwidthHz,
		bandwidthPresetsHz: [...p.bandwidthPresetsHz] as number[], bandwidthEditable: p.bandwidthEditable as boolean,
		inputRateHz: p.inputRateHz as number | null, minimumRateHz: p.minimumRateHz,
		audioCompanion: p.audioCompanion as string | null, sideband: p.sideband as PluginSideband,
		outputs: [...p.outputs] as PluginOutput[], common: p.common as boolean,
	};
}

/** Publish both tables atomically, sharing concurrent requests and allowing failed loads to retry. */
export async function loadPluginManifest(url = '/api/sdr/plugins'): Promise<PluginInfo[]> {
	if (manifest) return manifest;
	if (pending) return pending;
	pending = (async () => {
		const response = await fetch(url);
		if (!response.ok) throw new Error(`Receiver metadata: HTTP ${response.status}`);
		const body: unknown = await response.json();
		if (!body || typeof body !== 'object' || !('plugins' in body) || !('sstv_modes' in body)
			|| !Array.isArray(body.plugins) || !Array.isArray(body.sstv_modes)) throw new Error('Invalid receiver metadata');
		const plugins = body.plugins.map(parsePlugin);
		const identities = plugins.map((p) => `${p.kind === 'analog' || p.kind === 'digital' ? 'receiver' : p.kind}:${p.id}`);
		if (new Set(identities).size !== plugins.length) throw new Error('Duplicate receiver plugin');
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
export function isDigitalMode(id: string): boolean { return modeDescriptor(id)?.kind === 'digital'; }
export function audioStageIds(): string[] { return idsOf('audio'); }
export function ddcStageIds(): string[] { return idsOf('ddc'); }
export function sdrModeIds(): string[] { return [...analogModeIds(), ...digitalModeIds()]; }
export function modeDescriptor(id: string): ModeDescriptor | undefined {
	return manifest?.find((p): p is ModeDescriptor => p.id === id && (p.kind === 'analog' || p.kind === 'digital'));
}
export function modeOutputs(id: string): readonly PluginOutput[] { return modeDescriptor(id)?.outputs ?? []; }
export function modeStartupTimeoutMs(id: string): number {
	return modeDescriptor(id)?.startupTimeoutMs ?? 3000;
}
export function audioCompanionFor(id: string): string | null { return modeDescriptor(id)?.audioCompanion ?? null; }
export function isModeAvailable(id: string): boolean {
	const mode = modeDescriptor(id);
	return mode?.implemented === true && mode.available === true;
}
export function availableSdrModeIds(): string[] { return sdrModeIds().filter(isModeAvailable); }
export function setPluginManifest(next: PluginInfo[] | null): void { manifest = next; }
export function setSstvModeTable(next: SstvModeInfo[] | null): void { sstvModes = next; }
