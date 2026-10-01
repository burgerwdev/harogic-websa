/**
 * Store initialisation: the canvas binding and the HiDPI backing store.
 *
 * Importing core/store used to touch the DOM at module scope (report finding P2-1) and the
 * canvas had no device-pixel-ratio handling, so it was soft on HiDPI screens (P2-4). These
 * tests pin both: the binding happens in initStore(), drawing stays in logical 860x480
 * units, and the backing store is scaled by the device pixel ratio.
 */
import { describe, expect, it } from 'vitest';
import { getX, plotRect } from '../render/plot';
import { initStore, LOGICAL_H, LOGICAL_W, MARGIN, syncCanvasSize } from '../core/store';
import * as S from '../core/store';

const setDpr = (value: number) => {
	Object.defineProperty(window, 'devicePixelRatio', { value, configurable: true });
};

/** Give the bound (fake) canvas a CSS content box, as a laid-out element would have. */
const setBox = (w: number | null, h: number | null) => {
	const el = S.canvas as unknown as Record<string, unknown>;
	if (w === null || h === null) { delete el.clientWidth; delete el.clientHeight; return; }
	el.clientWidth = w;
	el.clientHeight = h;
};

describe('initStore', () => {
	it('scales the backing store but keeps logical drawing units', () => {
		setDpr(2);
		initStore();
		expect(S.pixelRatio).toBe(2);
		expect(S.canvas.width).toBe(LOGICAL_W * 2);
		expect(S.canvas.height).toBe(LOGICAL_H * 2);
		expect(S.W).toBe(LOGICAL_W);
		expect(S.H).toBe(LOGICAL_H);
		// geometry is unchanged: the left edge of the plot area is the margin
		expect(plotRect().x).toBe(MARGIN.left);
		expect(getX(0, 2)).toBe(MARGIN.left);
	});

	it('clamps the ratio and restores a 1:1 backing store on a normal display', () => {
		setDpr(1);
		initStore();
		expect(S.pixelRatio).toBe(1);
		expect(S.canvas.width).toBe(LOGICAL_W);
		expect(S.canvas.height).toBe(LOGICAL_H);
	});

	it('fails loudly when the canvas is missing', () => {
		const original = document.getElementById;
		document.getElementById = () => null;
		try {
			expect(() => initStore()).toThrow(/#spectrum is missing/);
		} finally {
			document.getElementById = original;
			initStore();
		}
	});
});

/**
 * The backing store is a function of the box the frame gives the canvas, so the plot stays
 * sharp from the laptop panel to a 4K window instead of being a fixed 860x480 bitmap that
 * CSS stretches (which is what "soft on a bigger screen" was).
 */
describe('syncCanvasSize', () => {
	it('derives the drawing space and backing store from the CSS box x DPR', () => {
		setDpr(2);
		setBox(1200, 700);
		try {
			initStore();
			expect(S.W).toBe(1200);
			expect(S.H).toBe(700);
			expect(S.pixelRatio).toBe(2);
			expect(S.canvas.width).toBe(2400);
			expect(S.canvas.height).toBe(1400);
			// geometry follows the new box: the plot area is the box minus the margins
			expect(plotRect().w).toBe(1200 - MARGIN.left - MARGIN.right);
		} finally {
			setBox(null, null);
			initStore();
		}
	});

	it('clamps a device scale factor below 1 up to 1 (an undersampled bitmap is blurrier)', () => {
		setDpr(0.906);   // what Chromium reports on the laptop panel with a desktop text scale
		setBox(1481, 708);
		try {
			initStore();
			expect(S.pixelRatio).toBe(1);
			expect(S.canvas.width).toBe(1481);
			expect(S.canvas.height).toBe(708);
		} finally {
			setBox(null, null);
			initStore();
		}
	});

	it('keeps the bitmap inside the pixel budget on a large window at 2x', () => {
		setDpr(2);
		setBox(3350, 1868);   // the plot column of a 3840x2160 window
		try {
			initStore();
			expect(S.pixelRatio).toBeGreaterThanOrEqual(1);
			expect(S.pixelRatio).toBeLessThan(2);
			expect(S.canvas.width * S.canvas.height).toBeLessThanOrEqual(8_100_000);
		} finally {
			setBox(null, null);
			initStore();
		}
	});

	it('reports no change when nothing moved (a resize observer must not loop)', () => {
		setDpr(1);
		setBox(900, 500);
		try {
			initStore();
			expect(syncCanvasSize()).toBe(false);
			// a real box change is reported once, and only once
			setBox(1000, 500);
			expect(syncCanvasSize()).toBe(true);
			expect(syncCanvasSize()).toBe(false);
		} finally {
			setBox(null, null);
			initStore();
		}
	});

	it('keeps the geometry in the drawing space units, at any box size', () => {
		// The readouts must not depend on the window: a point 10% into the plot is 10% into the
		// plot rectangle whatever the box is, and the amplitude axis stays inside the rectangle.
		setDpr(1);
		setBox(1200, 700);
		try {
			initStore();
			const r = plotRect();
			expect(r.x).toBe(MARGIN.left);
			expect(r.y).toBe(MARGIN.top);
			expect(r.w).toBe(1200 - MARGIN.left - MARGIN.right);
			expect(r.h).toBe(700 - MARGIN.top - MARGIN.bottom);
			expect(getX(0, 11)).toBe(r.x);
			expect(getX(10, 11)).toBe(r.x + r.w);
			expect(getX(5, 11)).toBe(r.x + r.w / 2);
		} finally {
			setBox(null, null);
			initStore();
		}
	});
});
