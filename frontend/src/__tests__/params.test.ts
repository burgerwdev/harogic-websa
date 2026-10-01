/**
 * Parameter slot semantics.
 *
 * These tests pin down the behaviour the rest of the UI now relies on: user intent wins
 * while it is in flight, an older reply cannot clobber it, a rejected command expires, and
 * persistence happens in exactly one place.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Param, allParams, createParam, resetAll } from '../core/params';

const slot = (over: Partial<Parameters<typeof createParam<number>>[1]> = {}) =>
	createParam<number>('test.value', { fallback: 0, scope: 'test', ...over });

beforeEach(() => {
	localStorage.clear();
	vi.useRealTimers();
});

describe('Param', () => {
	it('renders the fallback until the backend reports something', () => {
		const p = slot({ fallback: 42 });
		expect(p.get()).toBe(42);
		expect(p.confirmedValue()).toBeNull();
	});

	it('a user intent wins immediately and survives an in-flight reply', () => {
		const p = slot();
		p.confirm(10);
		p.set(30); // user asks for 30; the backend still reports 10 for a while
		expect(p.get()).toBe(30);
		expect(p.confirm(10)).toBe(false); // same value as before: no change
		expect(p.get()).toBe(30); // ...and the intent is NOT reverted
		expect(p.pending()).toBe(true);
	});

	it('a contradicting reply after a newer intent cannot win while the intent is fresh', () => {
		const p = slot();
		p.confirm(10);
		p.set(30);
		p.confirm(20); // an older command's reply arrives late
		expect(p.confirmedValue()).toBe(20);
		expect(p.get()).toBe(30);
		p.confirm(30); // backend caught up
		expect(p.pending()).toBe(false);
		expect(p.get()).toBe(30);
	});

	it('drops an intent the backend never accepts once the TTL expires', () => {
		vi.useFakeTimers();
		const p = slot({ fallback: 5, ttlMs: 1000 });
		p.confirm(10);
		p.set(30);
		expect(p.get()).toBe(30);
		vi.advanceTimersByTime(1500);
		p.confirm(10); // still 10: the command was rejected
		expect(p.pending()).toBe(false);
		expect(p.get()).toBe(10);
	});

	it('re-setting the same value keeps the original timestamp (no endless extension)', () => {
		vi.useFakeTimers();
		const p = slot({ ttlMs: 1000 });
		p.set(7);
		const epoch = p.epoch;
		vi.advanceTimersByTime(600);
		p.set(7);
		expect(p.epoch).toBe(epoch); // no new intent, no epoch bump
		expect(p.pendingAge()).toBe(600);
	});

	it('bumps the epoch on every distinct intent', () => {
		const p = slot();
		p.set(1);
		p.set(2);
		expect(p.epoch).toBe(2);
	});

	it('reset drops the intent without touching the confirmed value', () => {
		const p = slot();
		p.confirm(10);
		p.set(30);
		p.reset();
		expect(p.pending()).toBe(false);
		expect(p.get()).toBe(10);
		expect(p.confirmedValue()).toBe(10);
	});

	it('uses a custom equality (float tolerance)', () => {
		const p = createParam<number>('test.f', {
			fallback: 0,
			scope: 'test',
			equals: (a, b) => Math.abs(a - b) < 0.5,
		});
		p.set(10);
		p.confirm(10.2);
		expect(p.pending()).toBe(false); // 10.2 is "the same" as 10
	});
});

describe('Param persistence', () => {
	it("persists the confirmed value only, by default, and restores it on creation", () => {
		const p = createParam<number>('test.a', {
			fallback: 1,
			scope: 'test',
			persistKey: 'k',
			serialize: String,
			parse: Number,
		});
		p.set(9);
		expect(localStorage.getItem('k')).toBeNull(); // intent is not persisted yet
		p.confirm(9);
		expect(localStorage.getItem('k')).toBe('9');
		const again = createParam<number>('test.b', {
			fallback: 1,
			scope: 'test',
			persistKey: 'k',
			serialize: String,
			parse: Number,
		});
		expect(again.get()).toBe(9); // known before the first STATUS
	});

	it("persists user intent when persist: 'desired'", () => {
		const p = createParam<boolean>('test.flag', {
			fallback: false,
			scope: 'test',
			persistKey: 'flag',
			persist: 'desired',
		});
		p.set(true);
		expect(localStorage.getItem('flag')).toBe('true');
	});

	it('never throws when storage is unavailable', () => {
		const get = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('denied');
		});
		const set = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new Error('denied');
		});
		const p = createParam<number>('test.c', { fallback: 3, scope: 'test', persistKey: 'c' });
		p.confirm(4);
		expect(p.get()).toBe(4);
		get.mockRestore();
		set.mockRestore();
	});
});

describe('authoritative slots (client-owned preferences)', () => {
	it('never expires and never falls back while set', () => {
		vi.useFakeTimers();
		const p = createParam<boolean>('test.pref', {
			fallback: false, scope: 'test', authoritative: true,
			persistKey: 'pref', persist: 'desired',
			parse: (r) => r === '1', serialize: (v) => (v ? '1' : '0'),
		});
		p.set(true);
		vi.advanceTimersByTime(60_000);
		// A TTL here would silently revert the preference; a toggle reading the reverted
		// value can then only ever compute "on" (the SDR audio switch bug).
		expect(p.get()).toBe(true);
		expect(p.pending()).toBe(false);
		p.set(false);
		vi.advanceTimersByTime(60_000);
		expect(p.get()).toBe(false);
	});

	it('a new choice wins over the stored value (the bug my first test missed)', () => {
		localStorage.setItem('pref0', '0');
		const p = createParam<boolean>('test.pref0', {
			fallback: false, scope: 'test', authoritative: true,
			persistKey: 'pref0', persist: 'desired',
			parse: (r) => r === '1', serialize: (v) => (v ? '1' : '0'),
		});
		expect(p.get()).toBe(false); // restored
		p.set(true);
		// The restored value lives in `confirmed`; with confirmed-first ordering this
		// returned false and the UI could never show the user's choice.
		expect(p.get()).toBe(true);
		expect(localStorage.getItem('pref0')).toBe('1');
	});

	it('is restored from storage without any backend confirmation', () => {
		const p = createParam<boolean>('test.pref2', {
			fallback: false, scope: 'test', authoritative: true,
			persistKey: 'pref2', persist: 'desired',
			parse: (r) => r === '1', serialize: (v) => (v ? '1' : '0'),
		});
		p.set(true);
		const again = createParam<boolean>('test.pref3', {
			fallback: false, scope: 'test', authoritative: true,
			persistKey: 'pref2', parse: (r) => r === '1', serialize: (v) => (v ? '1' : '0'),
		});
		expect(again.get()).toBe(true);
	});
});

describe('resetAll', () => {
	it('clears every slot in a scope, and only that scope', () => {
		resetAll(); // start clean (slots registered by earlier tests)
		const sdr = createParam<number>('test.sdr', { fallback: 0, scope: 'sdr' });
		const swp = createParam<number>('test.swp', { fallback: 0, scope: 'swp' });
		sdr.confirm(1);
		swp.confirm(2);
		sdr.set(10);
		swp.set(20);
		resetAll('sdr');
		expect(sdr.get()).toBe(1); // intent dropped
		expect(swp.get()).toBe(20); // untouched
		resetAll();
		expect(swp.get()).toBe(2);
	});

	it('registers every created slot', () => {
		const before = allParams().length;
		createParam<number>('test.reg', { fallback: 0, scope: 'test' });
		expect(allParams().length).toBe(before + 1);
	});
});

describe('Param class shape', () => {
	it('exposes name/scope and keeps internals private', () => {
		const p = new Param<number>('n', { fallback: 0, scope: 's' });
		expect(p.name).toBe('n');
		expect(p.scope).toBe('s');
		expect(Object.keys(p)).not.toContain('_confirmed');
	});
});

describe('client-owned preferences stay authoritative (P1-6 / B4)', () => {
	it('does not revert when no backend report arrives (no TTL expiry)', async () => {
		const { harmValMode, peakListVisible, peakThrUserSet, normRefWinUser, valleySeqPos } =
			await import('../ui/measurePrefs');
		const { spanStepAuto, spanStepHz } = await import('../ui/swpState');
		const { unitMap, units } = await import('../core/units');

		// The display unit once reverted after the intent TTL because it was not marked
		// authoritative; the same trap applies to every client-owned preference.
		harmValMode.set('Peak');
		peakListVisible.set(true);
		peakThrUserSet.set(true);
		normRefWinUser.set(9);
		valleySeqPos.set(3);
		spanStepAuto.set(false);
		spanStepHz.set(2.5e6);
		unitMap.set({ ...units(), span: 'GHz' });

		vi.useFakeTimers();
		vi.setSystemTime(Date.now() + 60_000);      // far past any TTL
		expect(harmValMode.get()).toBe('Peak');
		expect(peakListVisible.get()).toBe(true);
		expect(peakThrUserSet.get()).toBe(true);
		expect(normRefWinUser.get()).toBe(9);
		expect(valleySeqPos.get()).toBe(3);
		expect(spanStepAuto.get()).toBe(false);
		expect(spanStepHz.get()).toBe(2.5e6);
		expect(units().span).toBe('GHz');
		vi.useRealTimers();
	});

	it('resetAll clears the pending values of a scope', async () => {
		const { harmValMode } = await import('../ui/measurePrefs');
		harmValMode.set('Avg');
		harmValMode.reset();
		expect(harmValMode.get()).toBe('RT');       // back to the declared fallback
	});
});
