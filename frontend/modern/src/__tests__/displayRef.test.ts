/**
 * Display-reference ownership.
 *
 * The value used to have nine writers with no arbitration, so a preset/normalise/trace
 * switch could silently clobber the SDR auto-scale. These tests pin the rule: while the SDR
 * auto-scale is enabled it owns the value, and everything else is a request that is ignored
 * (except an explicit user takeover, which turns the auto-scale off first).
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import { getDisplayRef, setDisplayRef } from '../ui/displayRef';
import { graphMode } from '../ui/graphMode';
import { sdrRefAuto } from '../ui/sdrState';

beforeEach(() => {
	resetAll();
	// resetAll() clears pending intents; make the confirmed mode explicit per test.
	graphMode.confirm('std');
});

describe('display reference ownership', () => {
	it('applies mode defaults in SWP/RTA even with the SDR auto-scale preference on', () => {
		sdrRefAuto.set(true); // preference only, the auto-scale runs in SDR only
		graphMode.confirm('std');
		expect(setDisplayRef('mode', -20)).toBe(true);
		expect(getDisplayRef()).toBe(-20);
	});

	it('ignores mode/preset requests while the SDR auto-scale owns the value', () => {
		graphMode.confirm('sdr');
		sdrRefAuto.set(true);
		setDisplayRef('auto', -15);
		expect(getDisplayRef()).toBe(-15);
		expect(setDisplayRef('mode', 0)).toBe(false);   // normalise must not fight it
		expect(setDisplayRef('preset', 0)).toBe(false); // nor a background reset
		expect(getDisplayRef()).toBe(-15);
	});

	it('lets the user take over, and the auto-scale resumes afterwards', () => {
		graphMode.confirm('sdr');
		sdrRefAuto.set(true);
		setDisplayRef('auto', -15);
		// The Ref Set button switches the auto-scale off before writing (see setRefLevel).
		sdrRefAuto.set(false);
		expect(setDisplayRef('user', -40)).toBe(true);
		expect(getDisplayRef()).toBe(-40);
		sdrRefAuto.set(true);
		setDisplayRef('auto', -20);
		expect(getDisplayRef()).toBe(-20);
	});

	it('applies mode defaults again once the SDR auto-scale is off', () => {
		graphMode.confirm('sdr');
		sdrRefAuto.set(false);
		expect(setDisplayRef('mode', 0)).toBe(true);
		expect(getDisplayRef()).toBe(0);
	});
});
