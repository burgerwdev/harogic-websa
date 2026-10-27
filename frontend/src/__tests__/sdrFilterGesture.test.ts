/**
 * Filter-edge gesture behaviour (ui/sdrFilterGesture.ts). Numeric/behavioural
 * assertions only; edges/preview/view values come from the module's own outputs against
 * the DSP band tables (demodBandHz mirrors dsp/core/src/analog/mod.rs).
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { setWS } from '../core/wsSend';
import { sdrDemod, sdrIfbw, sdrListenHz, sdrPitchHz } from '../ui/sdrState';
import { setPluginManifest } from '../sdr/registry';
import { filterPreviewHz } from '../ui/sdrFilter';
import { plotRect } from '../render/plot';
import {
	cancelFilterDrag,
	endFilterDrag,
	filterCenterAt,
	filterDragging,
	filterEdgeAt,
	filterGeometry,
	filterViewResolution,
	moveFilterDrag,
	startFilterDrag,
	viewSdrPassband,
} from '../ui/sdrFilterGesture';
import {
	__testSeedCapture,
	getCapture,
	getView,
	isZoomed,
	passbandViewActive,
	setView,
	zoomEnabled,
} from '../ui/spectrumViewport';

const MID_Y = 480 / 2;

const CAP = { lo: 9_800_000, hi: 10_200_000 };   // 400 kHz around 10 MHz -> 500 Hz/px
const LISTEN = 10_000_000;
const TOL = 8;

const sent: unknown[] = [];

function openSocket() {
	sent.length = 0;
	// Test rig: only readyState/send are exercised.
	const fakeSocket = {
		readyState: WebSocket.OPEN,
		send: (m: string) => sent.push(JSON.parse(m)),
	} as unknown as WebSocket;
	setWS(fakeSocket);
}

/** x of an absolute Hz in the window the plot draws (view when zoomed, else capture). */
function xOf(hz: number): number {
	const v = getView() ?? CAP;
	const p = plotRect();
	return p.x + (hz - v.lo) / (v.hi - v.lo) * p.w;
}

beforeEach(() => {
	resetAll();
	setPluginManifest([
		{ kind: 'analog', id: 'am', implemented: true, audioEnhancement: false },
		{ kind: 'analog', id: 'usb', implemented: true, audioEnhancement: false },
		{ kind: 'analog', id: 'lsb', implemented: true, audioEnhancement: false },
		{ kind: 'analog', id: 'cw', implemented: true, audioEnhancement: false },
		{ kind: 'digital', id: 'ft8', implemented: true, audioEnhancement: false },
	]);
	S.setSdrMode(true);
	sdrListenHz.set(LISTEN);
	sdrPitchHz.set(700);
	__testSeedCapture(CAP);
	openSocket();
});

afterEach(() => {
	cancelFilterDrag();
	setView(null, 1);
	S.setSdrMode(false);
	S.setFreqArray(null);
	__testSeedCapture(null);
	setWS(null);
});

describe('AM: symmetric total-bandwidth edges', () => {
	beforeEach(() => {
		sdrDemod.set('am');
		// 40 kHz total: edges +/-20 kHz = 40 px from the dial, beyond the overlap zones.
		sdrIfbw.set(40_000);
	});

	it('hits both edges at their true cutoff pixels', () => {
		expect(filterEdgeAt(xOf(LISTEN - 20_000), MID_Y, TOL)).toBe('low');
		expect(filterEdgeAt(xOf(LISTEN + 20_000), MID_Y, TOL)).toBe('high');
		expect(filterEdgeAt(xOf(LISTEN - 20_000) - TOL - 1, MID_Y, TOL)).toBeNull();
	});

	it('dragging one edge previews a symmetric total bandwidth', () => {
		expect(startFilterDrag('low', 1, xOf(LISTEN - 20_000))).toBe(true);
		expect(filterDragging()).toBe(true);
		// Low edge remains below the dial; the opposite edge mirrors its width.
		expect(moveFilterDrag(xOf(LISTEN - 2_000), 1)).toBe(true);
		expect(filterPreviewHz()).toBeCloseTo(4_000, 6);
		// the confirmed slot is untouched until the caller commits
		expect(sdrIfbw.get()).toBe(40_000);
		expect(endFilterDrag(1)).toBeCloseTo(4_000, 6);
		expect(filterPreviewHz()).toBeCloseTo(sdrIfbw.get(), 6);   // preview cleared
		expect(filterDragging()).toBe(false);
		expect(sent).toEqual([]);                                   // the gesture sends nothing
	});

	it('stops a low edge at minimum width when dragged beyond the centre', () => {
		expect(startFilterDrag('low', 1, xOf(LISTEN - 20_000))).toBe(true);
		moveFilterDrag(xOf(LISTEN + 2_000), 1);
		expect(filterPreviewHz()).toBe(50);
		expect(endFilterDrag(1)).toBe(50);
	});

	it('the y bound keeps the axis rows out of the edge hit', () => {
		const p = plotRect();
		expect(filterEdgeAt(xOf(LISTEN - 20_000), p.y - 1, TOL)).toBeNull();
		expect(filterEdgeAt(xOf(LISTEN - 20_000), p.y + p.h + 1, TOL)).toBeNull();
	});
});

describe('SSB: only the outer cutoff is a target', () => {
	it('USB keeps the guard edge inert and drags only the upper cutoff', () => {
		sdrDemod.set('usb');
		// 24 kHz: outer edge 48 px from the dial; guard = max(100, min(300, 2400)) = 300 Hz.
		sdrIfbw.set(24_000);
		expect(filterEdgeAt(xOf(LISTEN + 24_000), MID_Y, TOL)).toBe('high');
		expect(filterEdgeAt(xOf(LISTEN + 300), MID_Y, TOL)).toBeNull();   // guard is not a target
		// the inner (guard) edge cannot even start a drag
		expect(startFilterDrag('low', 1, xOf(LISTEN + 300))).toBe(false);
		expect(filterDragging()).toBe(false);
		// the outer edge previews the upper cutoff directly
		expect(startFilterDrag('high', 1, xOf(LISTEN + 24_000))).toBe(true);
		expect(moveFilterDrag(xOf(LISTEN + 3_000), 1)).toBe(true);
		expect(filterPreviewHz()).toBeCloseTo(3_000, 6);
		endFilterDrag(1);
	});

	it('LSB mirrors USB on the low side', () => {
		sdrDemod.set('lsb');
		sdrIfbw.set(24_000);
		expect(filterEdgeAt(xOf(LISTEN - 24_000), MID_Y, TOL)).toBe('low');
		expect(filterEdgeAt(xOf(LISTEN - 300), MID_Y, TOL)).toBeNull();
		expect(startFilterDrag('high', 1, xOf(LISTEN - 300))).toBe(false);
		expect(startFilterDrag('low', 1, xOf(LISTEN - 24_000))).toBe(true);
		expect(moveFilterDrag(xOf(LISTEN - 1_800), 1)).toBe(true);
		expect(filterPreviewHz()).toBeCloseTo(1_800, 6);
		endFilterDrag(1);
	});
});

describe('CW: the band sits around the sidetone pitch', () => {
	beforeEach(() => {
		sdrDemod.set('cw');
		sdrIfbw.set(500);
		// Zoom in so 5 Hz/px: band [450, 950] Hz -> edges 90/190 px from the dial.
		setView({ lo: LISTEN - 2_000, hi: LISTEN + 2_000 }, 1);
	});

	it('hits the pitch-relative edges, not dial-relative ones', () => {
		expect(filterEdgeAt(xOf(LISTEN + 450), MID_Y, TOL)).toBe('low');
		expect(filterEdgeAt(xOf(LISTEN + 950), MID_Y, TOL)).toBe('high');
		expect(filterEdgeAt(xOf(LISTEN - 250), MID_Y, TOL)).toBeNull();
	});

	it('dragging an edge keeps the band centred on the pitch', () => {
		startFilterDrag('high', 1, xOf(LISTEN + 950));
		// high edge to pitch + 500 -> width 1000 around the pitch.
		expect(moveFilterDrag(xOf(LISTEN + 700 + 500), 1)).toBe(true);
		expect(filterPreviewHz()).toBeCloseTo(1_000, 6);
		endFilterDrag(1);
	});
});

describe('narrow passbands: independent arrows without changing the real band', () => {
	it.each(['am', 'cw'])('%s keeps subpixel edges independently draggable without a jump', mode => {
		sdrDemod.set(mode);
		sdrIfbw.set(60);
		const geometry = filterGeometry()!;
		expect(geometry.highX! - geometry.lowX!).toBeLessThan(1);
		const [low, high] = geometry.handles;
		expect(high.x - low.x).toBeCloseTo(24, 6);
		for (const handle of [low, high]) {
			const startX = handle.x + (handle.edge === 'low' ? -5 : 5);
			expect(filterEdgeAt(startX, handle.y, TOL)).toBe(handle.edge);
			expect(startFilterDrag(handle.edge, 1, startX)).toBe(true);
			expect(filterPreviewHz()).toBe(60);
			expect(moveFilterDrag(startX, 1)).toBe(false);
			expect(filterPreviewHz()).toBe(60);
			const direction = handle.edge === 'low' ? -1 : 1;
			expect(moveFilterDrag(startX + direction * 2, 1)).toBe(true);
			expect(filterPreviewHz()).toBeGreaterThan(60);
			expect(moveFilterDrag(startX, 1)).toBe(true);
			expect(filterPreviewHz()).toBe(60);
			expect(endFilterDrag(1)).toBeNull();
		}
		expect(filterCenterAt(geometry.dialX, MID_Y, TOL)).toBe(true);
	});

	it('keeps both arrows reachable when a narrow band touches either plot boundary', () => {
		sdrDemod.set('am');
		sdrIfbw.set(60);
		for (const frequency of [CAP.lo + 30, CAP.hi - 30]) {
			sdrListenHz.set(frequency);
			const g = filterGeometry()!;
			expect(g.handles[1].x - g.handles[0].x).toBeCloseTo(24, 6);
			for (const handle of g.handles) {
				expect(handle.x).toBeGreaterThanOrEqual(g.plot.x);
				expect(handle.x).toBeLessThanOrEqual(g.plot.x + g.plot.w);
				expect(filterEdgeAt(handle.x, handle.y, TOL)).toBe(handle.edge);
			}
		}
	});

	it('zoom gives only the top arrows priority and keeps the background available', () => {
		sdrDemod.set('am');
		sdrIfbw.set(60);
		zoomEnabled.set(true);
		const g = filterGeometry()!;
		for (const handle of g.handles) {
			expect(filterEdgeAt(handle.x, handle.y, TOL)).toBe(handle.edge);
			expect(filterEdgeAt(handle.x, MID_Y, TOL)).toBeNull();
		}
		expect(filterEdgeAt(g.dialX, MID_Y, TOL)).toBeNull();
		zoomEnabled.set(false);
	});
});

describe('protocol modes are not edge-draggable', () => {
	it('a decoder band has no hit and no drag', () => {
		sdrDemod.set('ft8');
		sdrIfbw.set(3_000);
		for (const probe of [-1_000, 0, 1_500, 3_000, 4_500]) {
			expect(filterEdgeAt(xOf(LISTEN + probe), MID_Y, TOL)).toBeNull();
		}
		expect(startFilterDrag('high', 1, xOf(LISTEN + 3_000))).toBe(false);
		expect(filterDragging()).toBe(false);
	});
});

describe('ownership and cancellation', () => {
	beforeEach(() => {
		sdrDemod.set('am');
		sdrIfbw.set(40_000);
	});

	it('a move/end with a foreign pointer id is ignored', () => {
		startFilterDrag('high', 1, xOf(LISTEN + 20_000));
		expect(moveFilterDrag(xOf(LISTEN + 24_000), 2)).toBe(false);
		expect(filterPreviewHz()).toBeCloseTo(40_000, 6);   // nothing moved
		expect(endFilterDrag(2)).toBeNull();                // wrong owner ends nothing
		expect(filterDragging()).toBe(true);
		expect(endFilterDrag(1)).toBeNull();                // no preview change -> nothing to commit
	});

	it('a mode change under a drag cancels it and clears the preview', () => {
		startFilterDrag('high', 1, xOf(LISTEN + 20_000));
		moveFilterDrag(xOf(LISTEN + 24_000), 1);
		expect(filterPreviewHz()).toBeCloseTo(48_000, 6);
		sdrDemod.set('usb');
		expect(moveFilterDrag(xOf(LISTEN + 3_000), 1)).toBe(true);   // the cancellation repaint
		expect(filterDragging()).toBe(false);
		expect(filterPreviewHz()).toBeCloseTo(sdrIfbw.get(), 6);
	});

	it('a listen retune under a drag cancels it', () => {
		startFilterDrag('high', 1, xOf(LISTEN + 20_000));
		sdrListenHz.set(LISTEN + 5_000);
		expect(moveFilterDrag(xOf(LISTEN + 24_000), 1)).toBe(true);
		expect(filterDragging()).toBe(false);
	});

	it('a pitch change cancels a CW drag', () => {
		sdrDemod.set('cw');
		sdrIfbw.set(500);
		startFilterDrag('high', 1, xOf(LISTEN + 950));
		sdrPitchHz.set(600);
		expect(moveFilterDrag(xOf(LISTEN + 950), 1)).toBe(true);
		expect(filterDragging()).toBe(false);
	});

	it('cancel drops the preview without a value', () => {
		startFilterDrag('high', 1, xOf(LISTEN + 20_000));
		moveFilterDrag(xOf(LISTEN + 24_000), 1);
		expect(cancelFilterDrag()).toBe(true);
		expect(filterPreviewHz()).toBeCloseTo(sdrIfbw.get(), 6);
		expect(filterDragging()).toBe(false);
		expect(endFilterDrag(1)).toBeNull();
	});
});

describe('view passband: display-only, data-honest', () => {
	beforeEach(() => {
		sdrDemod.set('am');
		sdrIfbw.set(6_000);
	});

	it('toggles the band view without touching the acquisition or sending', () => {
		expect(viewSdrPassband()).toBe(true);
		const view = getView();
		expect(view).not.toBeNull();
		expect(view!.lo).toBeLessThan(LISTEN - 3_000);
		expect(view!.hi).toBeGreaterThan(LISTEN + 3_000);
		expect(zoomEnabled.get()).toBe(true);
		expect(passbandViewActive()).toBe(true);
		expect(viewSdrPassband()).toBe(true);
		expect(getView()).toEqual(CAP);
		expect(zoomEnabled.get()).toBe(false);
		expect(passbandViewActive()).toBe(false);
		expect(sent).toEqual([]);
		expect(getCapture()).toEqual(CAP);           // the drawn frame is unchanged
	});

	it('restores an existing local zoom exactly, even if the band is no longer usable', () => {
		const previous = { lo: LISTEN - 80_000, hi: LISTEN - 40_000 };
		setView(previous, 1);
		zoomEnabled.set(true);
		viewSdrPassband();
		expect(getView()).not.toEqual(previous);
		sdrListenHz.set(LISTEN + 1_000_000); // Restoration must precede band validation.
		expect(viewSdrPassband()).toBe(true);
		expect(getView()).toEqual(previous);
		expect(zoomEnabled.get()).toBe(true);
		expect(sent).toEqual([]);
	});

	it('clamps to >= 2 real sample steps of the on-screen axis (no interpolation)', () => {
		// The 100 Hz CW band sits around +700 Hz; 1 kHz bins cannot resolve it.
		S.setFreqArray(new Float64Array(101).map((_, i) => LISTEN - 50_000 + i * 1_000));
		sdrDemod.set('cw');
		sdrIfbw.set(100);
		viewSdrPassband();
		const view = getView()!;
		expect(view.hi - view.lo).toBeGreaterThanOrEqual(2_000);
		const res = filterViewResolution();
		expect(res).not.toBeNull();
		expect(res!.samples).toBeGreaterThan(1);     // real samples, not interpolated
		S.setFreqArray(null);
	});

	it('edge drags still hit after viewing the passband', () => {
		viewSdrPassband();
		expect(filterEdgeAt(xOf(LISTEN - 3_000), plotRect().y + 8, TOL)).toBe('low');
		expect(filterEdgeAt(xOf(LISTEN - 3_000), MID_Y, TOL)).toBeNull();
		setView(null, 1);
		expect(isZoomed()).toBe(false);
	});
});
