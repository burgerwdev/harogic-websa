/**
 * The spectrum/waterfall split: the divider's clamping, pinned without a layout.
 *
 * The drag itself is a couple of lines over `clampWfHeight`; what needs a real check is the range
 * it allows - the waterfall must not vanish under its own minimum, and it must never squeeze the
 * spectrum past its floor (the two ends the CSS cannot express because the column is fluid).
 */
import { describe, expect, it, beforeEach } from 'vitest';
import { clampWfHeight, parseWfHeight, resetWfSplit, syncWfSplit } from '../ui/panels/waterfall';
import { waterfallOn } from '../ui/waterfallState';

describe('the waterfall split', () => {
	it('passes a sane drag through', () => {
		expect(clampWfHeight(220, 900)).toBe(220);
	});

	it('never goes below the waterfall minimum', () => {
		expect(clampWfHeight(5, 900)).toBe(60);
		expect(clampWfHeight(-400, 900)).toBe(60);
	});

	it('keeps the spectrum its floor', () => {
		// 900 - 200 = 700, but 60% of the column caps it at 540 first.
		expect(clampWfHeight(2000, 900)).toBe(540);
		// A short window: the 60% cap wins (600 * 0.6 = 360 < 600 - 200 = 400).
		expect(clampWfHeight(2000, 600)).toBe(360);
	});

	it('survives a column with no layout yet', () => {
		expect(clampWfHeight(200, 0)).toBe(135);
	});

	it('reads a stored split, with an absent one meaning the default', () => {
		// Number(null) is 0 - a missing key once produced a zero-height waterfall.
		expect(parseWfHeight(null)).toBe(135);
		expect(parseWfHeight('')).toBe(135);
		expect(parseWfHeight('nonsense')).toBe(135);
		expect(parseWfHeight('420')).toBe(420);
	});

	describe('the Preset reset', () => {
		beforeEach(() => {
			document.body.innerHTML = '<div class="spectrum-area">'
				+ '<div id="wf-split"></div><div id="waterfall-container"></div></div>';
			localStorage.setItem('web-sa-wf-split', '420');
		});

		it('drops the dragged split and its stored value', () => {
			resetWfSplit();
			const container = document.getElementById('waterfall-container') as HTMLElement;
			expect(container.style.height).toBe('135px');
			expect(localStorage.getItem('web-sa-wf-split')).toBeNull();
		});

		it('hides the divider while the waterfall is off, shows it when on', () => {
			waterfallOn.set(false);
			syncWfSplit();
			const split = document.getElementById('wf-split') as HTMLElement;
			expect(split.style.display).toBe('none');
			waterfallOn.set(true);
			syncWfSplit();
			expect(split.style.display).toBe('');
			waterfallOn.set(false);
		});
	});
});
