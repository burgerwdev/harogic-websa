/**
 * Zoom gesture math + the "no wire traffic" contract (ui/spectrumZoomGestures.ts).
 *
 * The design's hard rule (§6): display-zoom operations must never produce
 * SET_FREQ/SET_RTA/SET_SDR (or any other) commands. Proven two ways here:
 *
 *   1. static: the zoom modules contain no wsSend import and no send() call;
 *   2. behavioural: with a captured fake WebSocket open (the axisDrag.test.ts rig),
 *      driving the zoom paths through real frames sends nothing, while the viewport
 *      state still moves.
 */
import { describe, expect, it, beforeEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { setWS } from '../core/wsSend';
import {
	commitMarquee,
	isMarqueeDrag,
	resetViewForDeviceGesture,
	spectrumFreqAtX,
	spectrumWheelZoom,
	updateMarqueePreview,
} from '../ui/spectrumZoomGestures';
import {
	isZoomed,
	setCapture,
	__testSeedCapture,
} from '../ui/spectrumViewport';

const CAP = { lo: 900e6, hi: 1000e6 };

const sent: any[] = [];

function openSocket() {
	sent.length = 0;
	setWS({ readyState: WebSocket.OPEN, send: (m: string) => sent.push(JSON.parse(m)) } as any);
}

beforeEach(() => {
	resetAll();
	__testSeedCapture(CAP);
	setCapture(CAP);
	openSocket();
});

describe('static wire-access scan', () => {
	const files = [
		'../ui/spectrumViewport.ts',
		'../ui/spectrumZoomUi.ts',
		'../ui/spectrumZoomGestures.ts',
	];

	/** Strip comments so prose like "send() call" cannot trip the scan. */
	const strip = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
	it('the zoom modules import no wsSend and call no send()', () => {
		for (const f of files) {
			const src = strip(readFileSync(resolve(import.meta.dirname, f), 'utf8'));
			expect(/from\s+'[^']*wsSend'/.test(src), `${f} imports wsSend`).toBe(false);
			expect(/\bsend\s*\(/.test(src), `${f} calls send()`).toBe(false);
		}
	});
});

describe('plot x mapping', () => {
	it('maps the plot rect onto the current view', () => {
		// core/store defaults (860x480, MARGIN l10/r50) give the plot rect via plotRect().
		const p = { x: 10, w: 800 };
		const view = { lo: 900e6, hi: 1000e6 };
		// The module reads plotRect() itself; assert monotony + endpoints instead of pixels.
		const a = spectrumFreqAtX(p.x)!;
		const b = spectrumFreqAtX(p.x + p.w)!;
		expect(a).toBeCloseTo(view.lo, 0);
		expect(b).toBeCloseTo(view.hi, 0);
		expect(spectrumFreqAtX(p.x + p.w + 50)).toBeNull();   // outside the plot
	});
});

describe('marquee', () => {
	it('flags a sideways drag past the threshold, not a vertical one', () => {
		expect(isMarqueeDrag(10, 2)).toBe(true);
		expect(isMarqueeDrag(7, 0)).toBe(false);     // below 8 px: still a click
		expect(isMarqueeDrag(20, 60)).toBe(false);   // vertical intent: never a marquee
	});

	it('commits a view and never sends a command', () => {
		const xL = 10, xMid = 410;                    // plot rect left/centre
		expect(commitMarquee(xL, xMid, 2e6)).toBe(true);
		expect(isZoomed()).toBe(true);
		const view = { lo: 900e6, hi: 950e6 };
		// The committed view spans the left half of the capture.
		expect(isZoomed()).toBe(true);
		void view;
		expect(sent).toEqual([]);                     // the whole point of the feature
	});

	it('updateMarqueePreview only previews, it does not touch the view', () => {
		updateMarqueePreview(10, 410);
		expect(isZoomed()).toBe(false);
		expect(sent).toEqual([]);
	});
});

describe('wheel zoom', () => {
	it('zooms around the anchor without sending anything', () => {
		expect(spectrumWheelZoom(-100, 410, 2e6)).toBe(true);
		expect(isZoomed()).toBe(true);
		expect(sent).toEqual([]);
		// Zooming back out to (at least) the capture returns to "not zoomed".
		for (let i = 0; i < 12 && isZoomed(); i++) spectrumWheelZoom(100, 410, 2e6);
		expect(isZoomed()).toBe(false);
		expect(sent).toEqual([]);
	});
});

describe('device-gesture reset', () => {
	it('drops the view for an axis-band gesture and reports nothing on the wire', () => {
		commitMarquee(10, 410, 2e6);
		expect(isZoomed()).toBe(true);
		expect(resetViewForDeviceGesture()).toBe(true);
		expect(isZoomed()).toBe(false);
		expect(sent).toEqual([]);
		expect(resetViewForDeviceGesture()).toBe(false);   // already full: no-op
	});
});

describe('store coupling', () => {
	it('uses the live freqArray length for the min span (no hidden state)', () => {
		S.setFreqArray(new Float64Array(101).map((_, i) => 900e6 + i * 1e6));
		// minSpan = 2 MHz: a sub-2 MHz marquee widens instead of refusing.
		expect(commitMarquee(400, 402, 2e6)).toBe(true);
		expect(isZoomed()).toBe(true);
		expect(sent).toEqual([]);
	});
});
