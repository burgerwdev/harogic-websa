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
import { initStore, LOGICAL_H, LOGICAL_W, MARGIN } from '../core/store';
import * as S from '../core/store';

const setDpr = (value: number) => {
	Object.defineProperty(window, 'devicePixelRatio', { value, configurable: true });
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
