/**
 * The FT8 ingress record: a decode lands in the log the window renders, and a stream reset clears it.
 *
 * The panel's one-line readout is gone (the decode table replaced it), so what matters is the log and
 * the diagnostic string the status line reads.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { lastFt8Message, renderFt8Message, resetSdrIq } from '../sdr/iqStream';
import { ft8Spots } from '../sdr/ft8Log';

const report = (over: Record<string, unknown> = {}) => ({
	text: 'CQ JO1WKO PM95',
	frequencyHz: 1000,
	timeOffsetS: 0.02,
	snrDb: 12,
	count: 1,
	centerHz: 21_074_000,
	...over,
});

describe('the FT8 decode record', () => {
	beforeEach(() => resetSdrIq());

	it('appends the decode to the log and keeps the diagnostic detail', () => {
		renderFt8Message(report());
		expect(ft8Spots().length).toBe(1);
		expect(ft8Spots()[0].text).toBe('CQ JO1WKO PM95');
		expect(lastFt8Message().text).toBe('CQ JO1WKO PM95');
		expect(lastFt8Message().detail).toContain('1000 Hz');
		expect(lastFt8Message().detail).toContain('+0.02 s');
		expect(lastFt8Message().count).toBe(1);
	});

	it('clears both the log and the detail on a stream reset', () => {
		renderFt8Message(report());
		expect(ft8Spots().length).toBe(1);
		resetSdrIq();
		expect(ft8Spots()).toEqual([]);
		expect(lastFt8Message().text).toBe('');
		expect(lastFt8Message().detail).toBe('');
	});
});
