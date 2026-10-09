/**
 * Preset cleanliness.
 *
 * Preset promises factory defaults. The persisted half of that promise lives in
 * resetSdrState(): resetAll('sdr') drops the in-memory intents, and the SDR_PREF_KEYS loop
 * removes the localStorage entries. A new SDR slot with a persistKey that is missing from
 * SDR_PREF_KEYS would survive every Preset and come back after a reload - exactly the bug
 * class the manual clearing had before the loop existed. These tests hold the line without
 * needing to read the slot's private options: they watch the storage itself.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { allParams, resetAll } from '../core/params';
import { resetSdrState } from '../ui/sdrState';

const sdrPersisted = (): string[] =>
	Object.keys(localStorage).filter((k) => k.startsWith('web-sa-sdr-'));

beforeEach(() => {
	localStorage.clear();
	resetAll('sdr');
});

describe('Preset clears the persisted SDR preferences', () => {
	it('every sdr-scope slot that persists leaves no storage behind', () => {
		// Write through every sdr slot the way the app does. Types differ per slot, so the
		// writes are deliberately untyped: the test only needs the persistence side effect.
		// set() persists the 'desired'-persisted slots, confirm() the backend-confirmed ones.
		const slots = allParams().filter((p) => p.scope === 'sdr');
		expect(slots.length).toBeGreaterThan(10); // the audit must see the real family
		for (const p of slots) {
			try { (p.set as unknown as (v: unknown) => void)(1); } catch { /* type rejects 1 */ }
			try { (p.confirm as unknown as (v: unknown) => boolean)(1); } catch { /* ditto */ }
		}
		resetSdrState();
		expect(sdrPersisted()).toEqual([]);
	});

	it('the new step memory is covered by the reset', () => {
		localStorage.setItem('web-sa-sdr-step-bw', '{"256":2500}');
		resetSdrState();
		expect(localStorage.getItem('web-sa-sdr-step-bw')).toBeNull();
	});

	it('storage the app does not own survives a Preset', () => {
		// Appearance and layout preferences (lang/theme/ui-scale/rail/panel) are not
		// measurement state: Preset must not touch them.
		localStorage.setItem('web-sa-theme', 'light');
		localStorage.setItem('web-sa-lang', 'zh');
		localStorage.setItem('web-sa-rail', 'collapsed');
		localStorage.setItem('web-sa-panel', 'hidden');
		resetSdrState();
		expect(localStorage.getItem('web-sa-theme')).toBe('light');
		expect(localStorage.getItem('web-sa-lang')).toBe('zh');
		expect(localStorage.getItem('web-sa-rail')).toBe('collapsed');
		expect(localStorage.getItem('web-sa-panel')).toBe('hidden');
	});
});
