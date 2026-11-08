/**
 * Global UI scale: the auto value a first visit picks, the stored override, and the one
 * conversion that matters to the canvas.
 *
 * The scale is applied as CSS `zoom` on the frame, so the canvas inside it covers `scale`
 * times more device pixels than its local clientWidth suggests - that is what core/store.ts
 * needs the scale for, and what these tests pin (the e2e asserts the same numbers in a real
 * browser: tools/e2e/viewport_baseline.py).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	UI_SCALE_AUTO, UI_SCALE_KEY, autoScale, chooseUiScale, getUiScale, initUiScale,
	initialScale, parseStoredScale, setUiScale,
} from '../core/uiScale';
import { initStore, syncCanvasSize } from '../core/store';
import * as S from '../core/store';

const setDpr = (value: number) => {
	Object.defineProperty(window, 'devicePixelRatio', { value, configurable: true });
};

const setBox = (w: number, h: number) => {
	const el = S.canvas as unknown as Record<string, unknown>;
	el.clientWidth = w;
	el.clientHeight = h;
};

afterEach(() => vi.restoreAllMocks());

describe('auto scale', () => {
	it('steps up with the viewport height and stays 1 on the panels this is designed for', () => {
		expect(autoScale(768)).toBe(1);      // laptop panel (the design baseline)
		expect(autoScale(920)).toBe(1);      // 2048x1536 at 1.6 (logical 1280x960)
		expect(autoScale(1080)).toBe(1.25);  // 1080p, and 4K at 200%
		expect(autoScale(1440)).toBe(1.5);
		expect(autoScale(2160)).toBe(2);     // 4K at 100%
	});
});

describe('stored override', () => {
	beforeEach(() => {
		localStorage.clear();
		setDpr(1);
		setUiScale(1);
	});

	it('ignores a value that is not an offered step', () => {
		expect(parseStoredScale('1.1')).toBeNull();
		expect(parseStoredScale('2')).toBe(2);
		expect(parseStoredScale(null)).toBeNull();
	});

	it('uses the screen value on a first visit and the stored one afterwards', () => {
		expect(initialScale(null, 2160)).toBe(2);
		expect(initialScale('1.25', 2160)).toBe(1.25);
		expect(initialScale('nonsense', 2160)).toBe(2);
	});

	it('remembers a manual pick across a reload, and Auto forgets it again', () => {
		chooseUiScale('1.5');
		expect(localStorage.getItem(UI_SCALE_KEY)).toBe('1.5');
		expect(initUiScale(localStorage.getItem(UI_SCALE_KEY))).toBe(1.5);   // "reload"
		expect(getUiScale()).toBe(1.5);

		const auto = chooseUiScale(UI_SCALE_AUTO);
		expect(localStorage.getItem(UI_SCALE_KEY)).toBeNull();
		expect(auto).toBe(autoScale());
	});

	it('applies the scale where CSS and the e2e can see it', () => {
		setUiScale(1.25);
		expect(document.documentElement.style.getPropertyValue('--ui')).toBe('1.25');
		expect(document.documentElement.dataset.uiScale).toBe('1.25');
	});

	it('initializes the scale and binds its control when storage reads are denied', () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new DOMException('denied', 'SecurityError'); });
		const select = document.createElement('select'); select.id = 'ui-scale';
		select.innerHTML = '<option value="auto">Auto</option><option value="1.5">1.5</option>';
		document.body.append(select);
		try {
			expect(() => initUiScale()).not.toThrow();
			expect(getUiScale()).toBe(autoScale());
			select.value = '1.5'; select.dispatchEvent(new Event('change'));
			expect(getUiScale()).toBe(1.5);
		} finally { select.remove(); }
	});

	it('applies a manual scale for this page when persistence is full', () => {
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError'); });
		expect(() => chooseUiScale('1.5')).not.toThrow();
		expect(getUiScale()).toBe(1.5);
	});

	it('returns to automatic scale when deleting the saved override is denied', () => {
		setUiScale(2);
		vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(() => { throw new DOMException('denied', 'SecurityError'); });
		expect(() => chooseUiScale(UI_SCALE_AUTO)).not.toThrow();
		expect(getUiScale()).toBe(autoScale());
	});
});

describe('canvas under a UI scale', () => {
	beforeEach(() => {
		localStorage.clear();
		setUiScale(1);
	});

	it('scales the backing store by DPR x UI scale (a zoomed canvas covers more device pixels)', () => {
		setDpr(1);
		setBox(900, 500);
		setUiScale(1.5);
		try {
			initStore();
			expect(S.pixelRatio).toBe(1.5);
			expect(S.canvas.width).toBe(1350);
			expect(S.canvas.height).toBe(750);
			// the drawing space stays in local CSS px, so the geometry is untouched by the zoom
			expect(S.W).toBe(900);
			expect(S.H).toBe(500);
		} finally {
			setUiScale(1);
			initStore();
		}
	});

	it('picks the scale up when it changes without the box moving', () => {
		setDpr(2);
		setBox(900, 500);
		try {
			initStore();
			expect(S.canvas.width).toBe(1800);
			setUiScale(1.25);
			// the ratio is part of the size comparison, so a scale change alone resizes
			expect(syncCanvasSize()).toBe(true);
			expect(S.canvas.width).toBe(2250);
		} finally {
			setUiScale(1);
			initStore();
		}
	});
});
