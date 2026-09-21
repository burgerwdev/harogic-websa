/**
 * The FT8 readout: what the panel shows after a decode.
 *
 * The decoder's correctness is covered elsewhere (the Rust fixture test and the artifact-level
 * test); this covers the last metre — that a decoded message reaches the panel with its slot
 * timing, and that a reset clears it.
 */
import { describe, expect, it } from 'vitest';
import { lastFt8Message, renderFt8Message, resetSdrIq } from '../sdr/iqStream';

const readout = (): HTMLElement => {
	const existing = document.getElementById('ft8-readout');
	if (existing) return existing as HTMLElement;
	const element = document.createElement('div');
	element.id = 'ft8-readout';
	document.body.appendChild(element);
	return element;
};

describe('the FT8 readout', () => {
	it('shows the decoded text with its frequency and slot timing', () => {
		readout();                       // the panel exists before a message arrives
		renderFt8Message({ text: 'CQ JO1WKO PM95', frequencyHz: 1000, timeOffsetS: 0.02, snrDb: 12, count: 1, centerHz: 21_074_000 });
		expect(readout().textContent).toContain('CQ JO1WKO PM95');
		expect(readout().textContent).toContain('1000 Hz');
		expect(readout().textContent).toContain('+0.02 s');
		expect(lastFt8Message().text).toBe('CQ JO1WKO PM95');
		expect(lastFt8Message().count).toBe(1);
	});

	it('keeps a count across messages and clears on reset', () => {
		renderFt8Message({ text: 'K1ABC W9XYZ EN37', frequencyHz: 1200, timeOffsetS: 0.01, snrDb: 5, count: 2, centerHz: 21_074_000 });
		expect(lastFt8Message().count).toBe(2);
		resetSdrIq();
		expect(lastFt8Message().text).toBe('');
		expect(lastFt8Message().count).toBe(0);
		expect(readout().textContent).toBe('—');
	});
});
