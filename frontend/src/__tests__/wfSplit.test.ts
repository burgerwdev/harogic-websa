/**
 * The spectrum/waterfall split: clamping, persistence and consecutive scaled drags.
 * A new grab must use the current border-box height, including when no spectrum frame arrives.
 */
import { describe, expect, it, beforeEach } from 'vitest';
import { clampWfHeight, initWfSplit, parseWfHeight, resetWfSplit, syncWfSplit } from '../ui/panels/waterfall';
import { waterfallOn } from '../ui/waterfallState';
import { setUiScale } from '../core/uiScale';

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
	it('keeps consecutive drags anchored to the border box at 125% without a new frame', () => {
		document.body.innerHTML = '<div class="spectrum-area">'
			+ '<div id="wf-split"></div><div id="waterfall-container"></div></div>';
		const area = document.querySelector('.spectrum-area') as HTMLElement;
		const split = document.getElementById('wf-split') as HTMLElement;
		const container = document.getElementById('waterfall-container') as HTMLElement;
		Object.defineProperty(area, 'clientHeight', { get: () => 720 });
		Object.defineProperties(container, {
			offsetHeight: { get: () => Number.parseFloat(container.style.height) },
			clientHeight: { get: () => Number.parseFloat(container.style.height) - 2 },
		});
		setUiScale(1.25);
		try {
			resetWfSplit();
			initWfSplit();
			const pointer = (type: string, y: number) => split.dispatchEvent(
				new PointerEvent(type, { clientY: y, pointerId: 1, bubbles: true }));
			pointer('pointerdown', 765);
			pointer('pointermove', 815);
			expect(container.offsetHeight).toBe(95); // 135 - 50 / 1.25, with no border loss
			pointer('pointerup', 815);
			pointer('pointerdown', 815);
			pointer('pointermove', 715);
			expect(container.offsetHeight).toBe(175); // the next grab starts from 95, not 93
			pointer('pointerup', 715);
			expect(localStorage.getItem('web-sa-wf-split')).toBe('175');
			// Movement after release must not resize the column.
			pointer('pointermove', 600);
			expect(container.offsetHeight).toBe(175);
		} finally {
			setUiScale(1);
			resetWfSplit();
		}
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
