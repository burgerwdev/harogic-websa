/**
 * The CW log: what the window needs to render a decode readably.
 *
 * Three facts arrive with each character and each is a decision, not a detail: the tone gate's share
 * (the confidence proxy - ggmorse reports none), the pause before it (a word gap, not a stuck page),
 * and the live level (the meter and the lamp, which must not re-render the text).
 */
import { describe, expect, it, beforeEach } from 'vitest';
import {
	addCwText, clearCwLog, cwLevel, cwLog, setCwLevel, subscribeCwLevel, subscribeCwLog,
} from '../sdr/cwLog';

describe('the CW decode log', () => {
	beforeEach(() => {
		clearCwLog();
		setCwLevel(0, false);
	});

	it('keeps one mark per character, with the share it was decoded at', () => {
		addCwText('AB', false, 0.7);
		addCwText('C', false, 0.2);
		const { current, marks } = cwLog();
		expect(current).toBe('ABC');
		expect(marks.map((m) => m.share)).toEqual([0.7, 0.7, 0.2]);
	});

	it('flags a word pause on the character that follows the decoded space', () => {
		// The pause comes from the text itself: ggmorse separates words with a space. Reported on the
		// bench: a timing rule dotted the inside of words, because the decoder reports a character
		// 100-300 ms after its keying and that lag adds to every gap it measures.
		addCwText('CQ DE', false, 0.7);
		const { marks } = cwLog();
		expect(marks.map((m) => m.gap)).toEqual([false, false, false, true, false]);
	});

	it('carries the rule across chunks and lines', () => {
		addCwText('CQ ', false, 0.7);            // a chunk may end on the space
		addCwText('DE', false, 0.7);             // ...so the next chunk's first character is the pause
		const { marks } = cwLog();
		expect(marks.map((m) => m.gap)).toEqual([false, false, false, true, false]);
	});

	it('closes the line on the decoder\'s own end-of-transmission mark', () => {
		addCwText('TEST', false, 0.7);
		addCwText(' DE', true, 0.7);
		const { lines, current, marks } = cwLog();
		expect(current).toBe('');
		expect(marks).toEqual([]);
		expect(lines).toHaveLength(1);
		expect(lines[0].text).toBe('TEST DE');
		expect(lines[0].marks).toHaveLength(7);
	});

	it('closes the line when the decoder reports the pause on its own', () => {
		// The pause arrives long after the last character, so the closing chunk carries no text. That
		// signal is the whole line break: dropping it ran every transmission into one line (reported).
		addCwText('TEST DE N0CALL', false, 0.7);
		addCwText('', true);
		const { lines, current } = cwLog();
		expect(current).toBe('');
		expect(lines).toHaveLength(1);
		expect(lines[0].text).toBe('TEST DE N0CALL');
	});

	it('keeps the level on its own listeners, so the meter cannot re-render the text', () => {
		let lineRenders = 0;
		let levelRenders = 0;
		const offLine = subscribeCwLog(() => { lineRenders++; });
		const offLevel = subscribeCwLevel(() => { levelRenders++; });
		addCwText('A', false, 0.7);           // a character: only the line store wakes up
		expect(lineRenders).toBe(1);
		expect(levelRenders).toBe(0);
		setCwLevel(0.2, true);                // a block's level: only the meter wakes up
		setCwLevel(0.3, false);
		expect(lineRenders).toBe(1);
		expect(levelRenders).toBe(2);
		offLine();
		offLevel();
		expect(cwLevel()).toEqual({ rms: 0.3, keyed: false });
	});
});
