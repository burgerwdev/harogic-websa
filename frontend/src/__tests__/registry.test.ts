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
	readSstvModeTable,
	PLUGIN_KINDS,
	setPluginManifest,
} from '../sdr/registry';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');

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

	it('declares SSTV as an implemented digital protocol', async () => {
		const plugins = await manifest();
		const sstv = plugins.find((p) => p.id === 'sstv');
		expect(sstv?.kind).toBe('digital');
		// The registry's flag and the build agree (the Rust test asserts the same thing); a listed
		// mode whose kernel is missing must show up as not implemented rather than as a crash.
		expect(sstv?.implemented).toBe(true);
	});

	describe('the SSTV mode table read from dsp.wasm', () => {
		it('lists every mode the decoder can build, with its VIS code and resolution', async () => {
			const modes = readSstvModeTable(await instantiateDsp(artifactBytes()));
			expect(modes).toHaveLength(18);
			// Dense indices in the table's own order: the mode pin and the highlight are indices.
			modes.forEach((mode, position) => {
				expect(mode.index).toBe(position);
				expect(mode.name.length).toBeGreaterThan(0);
				expect(mode.vis).toBeGreaterThan(0);
				expect(mode.vis).toBeLessThan(128);
				expect(mode.width).toBeGreaterThan(0);
				expect(mode.height).toBeGreaterThan(0);
			});
			expect(new Set(modes.map((mode) => mode.name)).size).toBe(modes.length);
			expect(new Set(modes.map((mode) => mode.vis)).size).toBe(modes.length);
		});

		it('matches the vendored timing table for known modes', async () => {
			const modes = readSstvModeTable(await instantiateDsp(artifactBytes()));
			const robot36 = modes.find((mode) => mode.name === 'Robot 36');
			expect(robot36).toMatchObject({ vis: 8, width: 320, height: 240 });
			const martinM1 = modes.find((mode) => mode.name === 'Martin M1');
			expect(martinM1).toMatchObject({ vis: 44, width: 320, height: 256 });
			const pd290 = modes.find((mode) => mode.name === 'PD-290');
			expect(pd290).toMatchObject({ width: 800, height: 616 });
		});
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
		const plugins = await manifest();
		setPluginManifest(plugins);
		expect(analogModeIds()).toEqual(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm']);
		expect(digitalModeIds()).toEqual(['ft8', 'drm', 'drmplus', 'sstv']);
		expect(ddcStageIds()).toContain('nco');
		expect(audioStageIds()).toContain('dc_block');
		// Availability is read from the manifest, not frozen here: every id the module reports as
		// implemented must be available, and an id nobody declares must not be. (Asserting a
		// specific stage's state would break every time a kernel lands.)
		for (const plugin of plugins) {
			expect(isModeAvailable(plugin.id)).toBe(plugin.implemented);
		}
		expect(plugins.some((p) => p.implemented)).toBe(true);
		expect(isModeAvailable('no_such_mode')).toBe(false);
		setPluginManifest(null);
	});

	it('returns an empty list before the manifest is loaded instead of inventing one', () => {
		setPluginManifest(null);
		expect(analogModeIds()).toEqual([]);
		expect(isModeAvailable('am')).toBe(false);
	});
});
