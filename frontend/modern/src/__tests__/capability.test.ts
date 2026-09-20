/**
 * Which DSP path runs: the browser (WASM) one, or the Python fallback.
 *
 * The decision is an explicit policy, not a side effect of whether a fetch worked, because the two
 * paths deliver audio through different sockets: a silent switch between them is indistinguishable
 * from "the radio stopped working", and the fallback has to be reachable on purpose (the e2e uses
 * `?wasm=0` to prove it still plays).
 */
import { describe, expect, it } from 'vitest';
import { wasmDspAllowed, wasmDspReason, wasmSupported, DSP_PREF_KEY } from '../sdr/capability';

const storageWith = (value: string | null) => ({ getItem: (key: string) => (key === DSP_PREF_KEY ? value : null) });

describe('the browser-DSP capability policy', () => {
	it('uses the browser DSP by default', () => {
		expect(wasmDspAllowed('', storageWith(null))).toBe(true);
		expect(wasmDspAllowed('?token=abc', storageWith(null))).toBe(true);
		expect(wasmDspReason('?token=abc')).toBe('');
	});

	it('falls back to Python when the operator asks for it', () => {
		for (const value of ['0', 'off', 'false']) {
			expect(wasmDspAllowed(`?wasm=${value}`, storageWith(null))).toBe(false);
		}
		expect(wasmDspReason('?wasm=0')).toBe('requested');
	});

	it('accepts an explicit opt-in even when a stored preference says otherwise', () => {
		expect(wasmDspAllowed('?wasm=1', storageWith('off'))).toBe(true);
	});

	it('honours a stored preference only when the URL does not decide', () => {
		expect(wasmDspAllowed('', storageWith('off'))).toBe(false);
		expect(wasmDspReason('', storageWith('off'))).toBe('stored-preference');
		expect(wasmDspAllowed('', storageWith('on'))).toBe(true);
	});

	it('is safe when storage is unreadable (private mode)', () => {
		const throwing = {
			getItem: () => {
				throw new Error('blocked');
			},
		};
		expect(wasmDspAllowed('', throwing)).toBe(true);
	});

	it('reports the WebAssembly check the pipeline depends on', () => {
		// Node has WebAssembly, so the pipeline can be exercised in this very test suite.
		expect(wasmSupported()).toBe(true);
	});
});
