/**
 * The FT8 decode log: what the window renders and what the click-to-tune reads.
 *
 * The log is the seam between the worker (one decode at a time) and the window (a table), so the
 * properties that matter here are the order (newest first, so a busy band does not need scrolling),
 * the absolute frequency (the channel's centre plus the audio offset - the number a click tunes to),
 * and that a long session cannot grow without bound.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	FT8_TUNE_HZ,
	MAX_FT8_SPOTS,
	addFt8Spot,
	clearFt8Spots,
	ft8DialFor,
	ft8Spots,
	subscribeFt8Spots,
	utcClock,
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
		// 14.074.000 + 1500 Hz: what a click tunes to, and what the table shows in MHz.
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

	it('tunes the dial so a clicked decode lands inside the decoder’s band', () => {
		// 21.075.500 is where the signal is; the decoder searches 200..3000 Hz of audio, so the dial
		// goes 1 kHz below it (a row clicked at DC would be outside the band entirely).
		expect(ft8DialFor(21_075_500)).toBe(21_074_500);
		expect(ft8DialFor(21_075_500) + FT8_TUNE_HZ).toBe(21_075_500);
	});

	it('formats the table clock in UTC', () => {
		// 2026-09-21T01:02:03Z, whatever the host's zone is.
		expect(utcClock(Date.UTC(2026, 8, 21, 1, 2, 3))).toBe('01:02:03');
	});
});
