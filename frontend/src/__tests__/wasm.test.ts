/**
 * WASM ABI contract test.
 *
 * Instantiates the *committed* artifact (`frontend/public/dsp.wasm`) with the real
 * loader and exercises the boundary the DSP will use: version gate, aligned allocation, typed
 * views, and growth not detaching the caller's block. This is what keeps the Rust ABI and the
 * TypeScript loader from drifting, without needing Rust in CI — the pure-Python
 * `tools/check_wasm_artifact.py` covers the export names and the recorded hash.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { DSP_ABI_VERSION, dspWasmUrl, instantiateDsp, loadDsp, wrapDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');

const artifactBytes = (): ArrayBuffer => {
	// A copy, so an instance never shares (and cannot grow) the fixture buffer.
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('dsp.wasm ABI', () => {
	it('is a real wasm module that the loader accepts', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		expect(dsp.exports.websa_dsp_version()).toBe(DSP_ABI_VERSION);
		expect(dsp.exports.memory).toBeInstanceOf(WebAssembly.Memory);
		expect(dsp.exports.websa_dsp_f32_bytes()).toBe(4);
		expect(dsp.exports.websa_dsp_block_align()).toBe(8);
	});

	it('allocates aligned, zeroed blocks and frees them', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		const ptr = dsp.alloc(4096);
		expect(ptr).toBeGreaterThan(0);
		expect(ptr % dsp.exports.websa_dsp_block_align()).toBe(0);
		const view = dsp.i16View(ptr, 8);
		expect(view.length).toBe(8);
		expect(Array.from(view)).toEqual([0, 0, 0, 0, 0, 0, 0, 0]);   // zeroed before the caller writes
		expect(() => dsp.free(ptr, 4096)).not.toThrow();
	});

	it('keeps a caller block valid when another allocation grows the memory', async () => {
		// The DSP holds one long-lived input block while other stages allocate; a detach would
		// silently turn that block into a zero-length view, so this is asserted, not assumed.
		const dsp = await instantiateDsp(artifactBytes());
		const first = dsp.alloc(64);
		expect(first).toBeGreaterThan(0);
		const big = dsp.alloc(1 << 20);
		expect(big).toBeGreaterThan(0);
		const view = dsp.f32View(first, 4);
		view[0] = 1.5;
		view[3] = -2.25;
		// Re-read through a fresh view: the block still holds what was written.
		expect(Array.from(dsp.f32View(first, 4))).toEqual([1.5, 0, 0, -2.25]);
		dsp.free(first, 64);
		dsp.free(big, 1 << 20);
	});

	it('rejects a module whose ABI version the loader does not know', async () => {
		const bytes = artifactBytes();
		const { instance } = await WebAssembly.instantiate(bytes, {});		const faked = {
			exports: new Proxy(instance.exports, {
				get: (target, prop) =>
					prop === 'websa_dsp_version'
						? () => DSP_ABI_VERSION + 1
						: (target as Record<string | symbol, unknown>)[prop],
			}),
		} as unknown as WebAssembly.Instance;
		expect(() => wrapDsp(faked)).toThrow(/ABI/);
	});

	it('fails loudly when the artifact is missing instead of running without a DSP', async () => {
		await expect(loadDsp('file:///nonexistent/dsp.wasm')).rejects.toThrow();
	});

	it('resolves the artifact next to the served page', () => {
		expect(dspWasmUrl('http://localhost:8080/static/dist/index.html'))
			.toBe('http://localhost:8080/static/dist/dsp.wasm');
	});
});
