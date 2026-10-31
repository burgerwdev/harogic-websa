import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setRenderer } from '../render/redraw';
import {
	__testSeedCapture, cancelReceiverFocus, getView, recenterOnCapture, resetViewport,
	setCapture, setView, slideViewTo, VIEW_SLIDE_MS,
} from '../ui/spectrumViewport';

const CAP = { lo: 0, hi: 1000 };
const START = { lo: 100, hi: 300 };
const TARGET = { lo: 600, hi: 800 };
let now: number, serial: number, reduced: boolean;
const frames = new Map<number, FrameRequestCallback>();
const render = vi.fn();

function advance(ms: number): void {
	now += ms;
	const pending = [...frames.values()];
	frames.clear();
	pending.forEach(frame => frame(now));
}

beforeEach(() => {
	now = serial = 0;
	reduced = false;
	frames.clear();
	render.mockClear();
	vi.spyOn(performance, 'now').mockImplementation(() => now);
	vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
	vi.stubGlobal('matchMedia', () => ({ matches: reduced }));
	vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
		frames.set(++serial, callback);
		return serial;
	});
	vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
	__testSeedCapture(CAP);
	setView(START, 1);
	setRenderer(render);
});

afterEach(() => {
	resetViewport();
	setRenderer(null);
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('spectrum view sliding', () => {
	it('moves smoothly, slows near the target and keeps the zoom width', () => {
		slideViewTo(TARGET, 1);
		expect(getView()).toEqual(START);
		advance(VIEW_SLIDE_MS / 3);
		const first = { ...getView()! };
		expect(first.lo).toBeGreaterThan(START.lo);
		expect(first.lo).toBeLessThan(TARGET.lo);
		expect(first.hi - first.lo).toBeCloseTo(200);
		advance(VIEW_SLIDE_MS / 3);
		const second = getView()!;
		expect(second.lo).toBeGreaterThan(first.lo);
		expect(second.lo - first.lo).toBeLessThan(first.lo - START.lo);
		expect(second.hi - second.lo).toBeCloseTo(200);
		advance(VIEW_SLIDE_MS / 3);
		expect(getView()).toEqual(TARGET);
		expect(frames.size).toBe(0);
		expect(render).toHaveBeenCalledTimes(3);
	});

	it('retargets from the position currently shown without jumping', () => {
		slideViewTo(TARGET, 1);
		advance(80);
		const shown = { ...getView()! };
		const reverse = { lo: 0, hi: 200 };
		slideViewTo(reverse, 1);
		expect(getView()).toEqual(shown);
		advance(80);
		expect(getView()!.lo).toBeLessThan(shown.lo);
		advance(VIEW_SLIDE_MS);
		expect(getView()).toEqual(reverse);
	});

	it('does not restart when the same destination is requested again', () => {
		slideViewTo(TARGET, 1);
		advance(80);
		slideViewTo(TARGET, 1);
		advance(VIEW_SLIDE_MS - 80);
		expect(getView()).toEqual(TARGET);
	});

	it('clamps the whole slide at the capture boundary', () => {
		slideViewTo({ lo: 900, hi: 1100 }, 1);
		for (let i = 0; i < 4; i++) {
			advance(VIEW_SLIDE_MS / 4);
			expect(getView()!.lo).toBeGreaterThanOrEqual(CAP.lo);
			expect(getView()!.hi).toBeLessThanOrEqual(CAP.hi);
			expect(getView()!.hi - getView()!.lo).toBeCloseTo(200);
		}
		expect(getView()).toEqual({ lo: 800, hi: 1000 });
	});

	it('lets direct view input replace an animation and ignores its stale callback', () => {
		slideViewTo(TARGET, 1);
		const stale = [...frames.values()][0];
		advance(80);
		const manual = { lo: 400, hi: 600 };
		setView(manual, 1);
		stale(VIEW_SLIDE_MS);
		advance(VIEW_SLIDE_MS);
		expect(getView()).toEqual(manual);
		expect(frames.size).toBe(0);
	});

	it('freezes the displayed position when a new receiver or axis operation takes over', () => {
		slideViewTo(TARGET, 1);
		advance(80);
		const shown = { ...getView()! };
		cancelReceiverFocus();
		advance(VIEW_SLIDE_MS);
		expect(getView()).toEqual(shown);
		expect(frames.size).toBe(0);
	});

	it('continues through unchanged frames and cancels on a capture bandwidth change', () => {
		slideViewTo(TARGET, 1);
		advance(80);
		const first = getView()!.lo;
		setCapture(CAP);
		advance(80);
		expect(getView()!.lo).toBeGreaterThan(first);
		setCapture({ lo: 0, hi: 500 });
		advance(VIEW_SLIDE_MS);
		expect(getView()).toEqual({ lo: 0, hi: 500 });
		expect(frames.size).toBe(0);
	});

	it('starts a deferred focus only after its matching capture arrives', () => {
		recenterOnCapture(2700, { lo: 2000, hi: 3000 }, 1);
		expect(getView()).toEqual(START);
		expect(frames.size).toBe(0);
		setCapture({ lo: 2000, hi: 3000 });
		expect(getView()).toEqual({ lo: 2100, hi: 2300 });
		advance(80);
		expect(getView()!.lo).toBeGreaterThan(2100);
		expect(getView()!.lo).toBeLessThan(2600);
		advance(VIEW_SLIDE_MS);
		expect(getView()).toEqual({ lo: 2600, hi: 2800 });
	});

	it('resets without leaving an animation that can restore an old window', () => {
		slideViewTo(TARGET, 1);
		resetViewport();
		advance(VIEW_SLIDE_MS);
		expect(getView()).toBeNull();
		expect(frames.size).toBe(0);
	});

	it('respects reduced motion both before and during a slide', () => {
		reduced = true;
		slideViewTo(TARGET, 1);
		expect(getView()).toEqual(TARGET);
		expect(frames.size).toBe(0);
		reduced = false;
		slideViewTo(START, 1);
		advance(80);
		reduced = true;
		advance(16);
		expect(getView()).toEqual(START);
		expect(frames.size).toBe(0);
	});
});
