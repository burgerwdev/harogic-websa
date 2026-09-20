/**
 * The SDR mode buttons come from the DSP registry, not from `index.html`.
 *
 * The panel used to carry a hardcoded button list, which is how a mode could exist in the DSP
 * while the UI never offered it (or the other way round). These tests pin the new rule: the group
 * is rendered from the manifest, an unimplemented mode is disabled rather than silently accepted,
 * and `index.html` no longer contains any mode buttons.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';
import { renderSdrDemodGroup } from '../ui/sdrDemodGroup';
import { readPluginManifest, setPluginManifest } from '../sdr/registry';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', '..', 'frontend', 'modern', 'public', 'dsp.wasm');
const INDEX_HTML = resolve(process.cwd(), 'index.html');

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('the SDR demodulator group', () => {
	it('is generated from the manifest, with unimplemented modes disabled', async () => {
		setPluginManifest(readPluginManifest(await instantiateDsp(artifactBytes())));
		const container = document.createElement('div');
		const selected: string[] = [];
		// 'am' is implemented; an id the registry does not know cannot be run, so it must be
		// disabled. Asserting the *property* (disabled iff unavailable) keeps this test true as
		// modes land, instead of freezing today's set of finished kernels.
		renderSdrDemodGroup(container, ['am', 'not_a_mode'], { onSelect: (id) => selected.push(id) });

		const buttons = Array.from(container.querySelectorAll('button'));
		expect(buttons.map((b) => b.dataset.sdrDemod)).toEqual(['am', 'not_a_mode']);
		expect(buttons.map((b) => b.textContent)).toEqual(['AM', 'NOT_A_MODE']);
		expect(buttons[0].disabled).toBe(false);
		expect(buttons[1].disabled).toBe(true);
		expect(buttons[1].title).toMatch(/not implemented/);

		buttons[0].click();
		expect(selected).toEqual(['am']);
		buttons[1].click();
		expect(selected).toEqual(['am']);          // a disabled mode reports nothing
		setPluginManifest(null);
	});

	it('enables exactly the modes the registry reports as implemented', async () => {
		const manifest = readPluginManifest(await instantiateDsp(artifactBytes()));
		setPluginManifest(manifest);
		const analog = manifest.filter((p) => p.kind === 'analog');
		const container = document.createElement('div');
		renderSdrDemodGroup(container, analog.map((p) => p.id), { onSelect: () => {} });
		for (const button of Array.from(container.querySelectorAll('button'))) {
			const expected = analog.find((p) => p.id === button.dataset.sdrDemod);
			expect(button.disabled).toBe(!expected?.implemented);
		}
		setPluginManifest(null);
	});

	it('offers exactly the analog modes the module declares', async () => {
		const manifest = readPluginManifest(await instantiateDsp(artifactBytes()));
		setPluginManifest(manifest);
		const analog = manifest.filter((p) => p.kind === 'analog').map((p) => p.id);
		const container = document.createElement('div');
		renderSdrDemodGroup(container, analog, { onSelect: () => {} });
		const rendered = Array.from(container.querySelectorAll('button')).map((b) => b.dataset.sdrDemod);
		expect(rendered).toEqual(analog);
		expect(rendered).toContain('nfm');
		expect(rendered).toContain('pm');
		setPluginManifest(null);
	});

	it('has no hardcoded mode buttons left in index.html', () => {
		// The structural half of the rule: the markup cannot reintroduce a second list.
		const html = readFileSync(INDEX_HTML, 'utf8');
		expect(html).not.toMatch(/data-sdr-demod=/);
		expect(html).toMatch(/id="sdr-demod-group"/);
	});

	it('renders nothing when the registry has not loaded, instead of guessing', () => {
		setPluginManifest(null);
		const container = document.createElement('div');
		const onSelect = vi.fn();
		renderSdrDemodGroup(container, [], { onSelect });
		expect(container.querySelectorAll('button')).toHaveLength(0);
	});
});
