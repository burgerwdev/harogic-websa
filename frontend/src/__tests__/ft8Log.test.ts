/**
 * The FT8 decode log: the retained messages the window renders.
 *
 * The log is the seam between the worker (one decode at a time) and the window (a table), so the
 * properties that matter here are the order (newest first, so a busy band does not need scrolling),
 * the absolute frequency (the channel's centre plus the audio offset),
 * and that a long session cannot grow without bound.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	MAX_FT8_SPOTS,
	addFt8Spot,
	clearFt8Spots,
	ft8Spots,
	subscribeFt8Spots,
	clock,
} from '../sdr/ft8Log';
import type { Ft8Report } from '../sdr/types';

const report = (over: Partial<Ft8Report> = {}): Ft8Report => ({
	text: 'CQ JO1WKO PM95',
	frequencyHz: 1000,
	timeOffsetS: 0.02,
	snrDb: 12,
	count: 1,
	centerHz: 21_074_000,
	...over,
});

describe('the FT8 decode log', () => {
	beforeEach(() => clearFt8Spots());

	it('keeps the newest decode first and computes the absolute frequency', () => {
		addFt8Spot(report({ text: 'first', frequencyHz: 900, centerHz: 21_074_000 }), 1_000);
		addFt8Spot(report({ text: 'second', frequencyHz: 1500, centerHz: 21_074_000 }), 2_000);
		const spots = ft8Spots();
		expect(spots.map((s) => s.text)).toEqual(['second', 'first']);
		// The table shows the dial plus audio offset in MHz.
		expect(spots[0].hz).toBe(21_075_500);
		expect(spots[0].offsetHz).toBe(1500);
		expect(spots[1].hz).toBe(21_074_900);
	});

	it('notifies subscribers and stops when they unsubscribe', () => {
		const seen = vi.fn();
		const stop = subscribeFt8Spots(seen);
		addFt8Spot(report(), 1_000);
		expect(seen).toHaveBeenCalledTimes(1);
		clearFt8Spots();
		expect(seen).toHaveBeenCalledTimes(2);
		expect(ft8Spots()).toEqual([]);
		stop();
		addFt8Spot(report(), 2_000);
		expect(seen).toHaveBeenCalledTimes(2);
	});

	it('caps a long session instead of growing', () => {
		for (let index = 0; index < MAX_FT8_SPOTS + 25; index++) {
			addFt8Spot(report({ text: `m${index}` }), index);
		}
		const spots = ft8Spots();
		expect(spots.length).toBe(MAX_FT8_SPOTS);
		expect(spots[0].text).toBe(`m${MAX_FT8_SPOTS + 24}`);   // the newest survived
		expect(spots[spots.length - 1].text).toBe('m25');        // the oldest were dropped
	});

	it('formats the clock in UTC or local time', () => {
		const at = Date.UTC(2026, 8, 21, 1, 2, 3);
		const prevTz = process.env.TZ;
		try {
			process.env.TZ = 'Etc/GMT+5';   // a fixed -5 zone, no DST
			expect(clock(at, true)).toBe('01:02:03');
			expect(clock(at, false)).toBe('20:02:03');
		} finally {
			process.env.TZ = prevTz;
		}
	});
});

it('keeps unavailable FT8 SNR null and preserves negative 2500 Hz reports', () => {
  clearFt8Spots();
  expect(addFt8Spot(report({ snrDb: null })).snrDb).toBeNull();
  expect(addFt8Spot(report({ snrDb: -21 })).snrDb).toBe(-21);
});
