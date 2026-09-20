/**
 * Plugin registry contract test.
 *
 * The registry is read out of the *committed* DSP artifact over the ABI, which is what makes the
 * "one list of modes" rule real: this test fails if the module's manifest cannot be read, if a
 * documented mode disappears from it, or if a stage is claimed as implemented while the registry
 * says otherwise. It is the TypeScript half of
 * `wasm/tests/path_separation.rs::every_registered_plugin_can_be_resolved_by_its_own_family`.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
	analogModeIds,
	audioStageIds,
	ddcStageIds,
	digitalModeIds,
	isModeAvailable,
	readPluginManifest,
	PLUGIN_KINDS,
	setPluginManifest,
} from '../sdr/registry';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', '..', 'frontend', 'modern', 'public', 'dsp.wasm');

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

async function manifest() {
	return readPluginManifest(await instantiateDsp(artifactBytes()));
}

describe('the plugin manifest read from dsp.wasm', () => {
	it('lists every family the architecture defines', async () => {
		const plugins = await manifest();
		expect(plugins.length).toBeGreaterThan(0);
		for (const plugin of plugins) {
			expect(PLUGIN_KINDS).toContain(plugin.kind);
			expect(plugin.id).toMatch(/^[a-z0-9_]+$/);
			// An id may not appear twice inside a family: the UI keys off it.
			const sameKind = plugins.filter((p) => p.kind === plugin.kind && p.id === plugin.id);
			expect(sameKind).toHaveLength(1);
		}
		for (const kind of PLUGIN_KINDS) {
			expect(plugins.some((plugin) => plugin.kind === kind)).toBe(true);
		}
	});

	it('declares every analog mode the architecture promises', async () => {
		const plugins = await manifest();
		const analog = plugins.filter((p) => p.kind === 'analog').map((p) => p.id);
		expect(analog).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm']);
	});

	it('declares FT8 as the first digital protocol', async () => {
		const plugins = await manifest();
		const digital = plugins.filter((p) => p.kind === 'digital').map((p) => p.id);
		expect(digital).toContain('ft8');
	});

	it('marks the DDC stages as shared base-layer plugins, not audio enhancement', async () => {
		const plugins = await manifest();
		const ddc = plugins.filter((p) => p.kind === 'ddc');
		expect(ddc.map((p) => p.id)).toEqual(['nco', 'fir', 'decimate', 'resample', 'agc']);
		// The DDC is shared by both paths, so it must never be marked as audio enhancement: only
		// the analog PCM chain may be.
		expect(ddc.every((p) => !p.audioEnhancement)).toBe(true);
	});

	it('only marks audio stages as audio enhancement', async () => {
		const plugins = await manifest();
		for (const plugin of plugins) {
			expect(plugin.audioEnhancement).toBe(plugin.kind === 'audio');
		}
	});

	it('reports the DC blocker as implemented (it is the first real chain stage)', async () => {
		const plugins = await manifest();
		const dcBlock = plugins.find((p) => p.id === 'dc_block');
		expect(dcBlock?.kind).toBe('audio');
		expect(dcBlock?.implemented).toBe(true);
	});

	it('exposes family accessors backed by the same manifest', async () => {
		setPluginManifest(await manifest());
		expect(analogModeIds()).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm']);
		expect(digitalModeIds()).toEqual(['ft8']);
		expect(ddcStageIds()).toContain('nco');
		expect(audioStageIds()).toContain('dc_block');
		// A declared-but-unwritten mode is listed and reported as unavailable, never as usable.
		expect(audioStageIds()).toContain('wiener');
		expect(isModeAvailable('dc_block')).toBe(true);
		expect(isModeAvailable('wiener')).toBe(false);
		expect(isModeAvailable('no_such_mode')).toBe(false);
		setPluginManifest(null);
	});

	it('returns an empty list before the manifest is loaded instead of inventing one', () => {
		setPluginManifest(null);
		expect(analogModeIds()).toEqual([]);
		expect(isModeAvailable('am')).toBe(false);
	});
});
