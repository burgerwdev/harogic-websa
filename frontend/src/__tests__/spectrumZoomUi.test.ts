/**
 * The top-bar zoom toggle + overview widget state machine (ui/spectrumZoomUi.ts).
 *
 * Pins the design §3 contract: the button mirrors the keypad button's convention
 * (.active + aria-pressed) but nothing else; the overview box follows the toggle;
 * Off returns to the full window; PNM disables the whole UI without touching the
 * user's toggle choice; none of it sends anything.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { resetAll } from '../core/params';
import { deviceMode } from '../ui/graphMode';
import { syncGraphModeStatus } from '../ui/controls';
import { getCapture, getView, sameWindow, setCapture, setView, togglePassbandView, zoomEnabled, __testSeedCapture } from '../ui/spectrumViewport';
import {
	initSpectrumZoomUi,
	isSpectrumZoomOn,
	spectrumZoomAvailable,
	syncSpectrumZoomUi,
	toggleSpectrumZoom,
} from '../ui/spectrumZoomUi';
import * as S from '../core/store';
import { initControlRail } from '../ui/controlRail';
import { commitMarquee } from '../ui/spectrumZoomGestures';
import { clearDisplayBounds, getDisplayRange, getDisplayRef, getDisplayTop, hasDisplayBounds, noteDisplayRefReport } from '../ui/displayRef';
import { refLevel } from '../ui/refState';
import { ampAtten, ampPreamp } from '../ui/ampState';
import { send } from '../core/wsSend';
import type * as WsSendModule from '../core/wsSend';
import { axisDragMove, axisDragStart, resetAxisPreview } from '../ui/axisDrag';
import { graphMode } from '../ui/graphMode';

vi.mock('../core/wsSend', async (importOriginal) => {
	const actual = await importOriginal<typeof WsSendModule>();
	return { ...actual, send: vi.fn() };
});

const CAP = { lo: 900e6, hi: 1000e6 };

function buildDom(): void {
	document.body.innerHTML = `
		<div class="info-right">
			<button class="btn bar-btn" id="btn-spectrum-zoom" aria-pressed="false"></button>
		</div>
		<button class="btn" id="btn-sdr-filter-view" aria-pressed="false"></button>
		<div class="spectrum-area">
			<div id="zoom-overview-box" style="display:none;">
				<div class="zoom-overview-head">
					<span id="zoom-overview-title"></span>
					<button class="btn zoom-reset-btn" id="btn-zoom-reset" disabled></button>
					<button class="btn zoom-reset-btn" id="btn-zoom-close"></button>
				</div>
				<canvas id="zoom-overview" width="300" height="48"></canvas>
				<div id="zoom-overview-hint" style="display:none;"></div>
			</div>
		</div>`;
}

function btn(): HTMLButtonElement {
	return document.getElementById('btn-spectrum-zoom') as HTMLButtonElement;
}

function box(): HTMLElement {
	return document.getElementById('zoom-overview-box')!;
}

beforeEach(() => {
	document.body.innerHTML = '';
	resetAll();
	resetAxisPreview();
	__testSeedCapture(null);
	buildDom();
	initSpectrumZoomUi();
});

describe('the passband view action', () => {
	it('keeps the passband label stable while exposing pressed and restore semantics', () => {
		__testSeedCapture(CAP);
		const filterButton = document.getElementById('btn-sdr-filter-view') as HTMLButtonElement;
		syncSpectrumZoomUi(true);
		const label = filterButton.textContent;
		const viewDescription = filterButton.getAttribute('aria-label');
		togglePassbandView({ lo: 940e6, hi: 960e6 }, 2e6);
		syncSpectrumZoomUi(true);
		expect(filterButton.textContent).toBe(label);
		expect(filterButton.classList.contains('active')).toBe(true);
		expect(filterButton.getAttribute('aria-pressed')).toBe('true');
		expect(filterButton.getAttribute('aria-label')).not.toBe(viewDescription);
		expect(filterButton.disabled).toBe(false);
		togglePassbandView({ lo: 940e6, hi: 960e6 }, 2e6);
		syncSpectrumZoomUi(true);
		expect(filterButton.textContent).toBe(label);
		expect(filterButton.classList.contains('active')).toBe(false);
		expect(filterButton.getAttribute('aria-pressed')).toBe('false');
		expect(filterButton.getAttribute('aria-label')).toBe(viewDescription);
	});
});
describe('the toggle state machine', () => {
	it('starts Off with the overview hidden', () => {
		expect(zoomEnabled.get()).toBe(false);
		expect(btn().classList.contains('active')).toBe(false);
		expect(btn().getAttribute('aria-pressed')).toBe('false');
		expect(box().style.display).toBe('none');
		expect(isSpectrumZoomOn()).toBe(false);
	});

	it('On shows the overview and the waiting title without a capture', () => {
		toggleSpectrumZoom();
		expect(zoomEnabled.get()).toBe(true);
		expect(btn().classList.contains('active')).toBe(true);
		expect(btn().getAttribute('aria-pressed')).toBe('true');
		expect(box().style.display).toBe('');
	});

	it('Off returns to the full window (the view is dropped)', () => {
		__testSeedCapture(CAP);
		setCapture(CAP);
		toggleSpectrumZoom();              // On
		toggleSpectrumZoom();              // Off again
		expect(isSpectrumZoomOn()).toBe(false);
		expect(box().style.display).toBe('none');
	});

	it('the overview close button exits the whole mode', () => {
		toggleSpectrumZoom();
		expect(isSpectrumZoomOn()).toBe(true);
		document.getElementById('btn-zoom-close')!.click();
		expect(isSpectrumZoomOn()).toBe(false);
		expect(btn().classList.contains('active')).toBe(false);
		expect(box().style.display).toBe('none');
	});

	it('syncs the restore button with the zoomed state', () => {
		__testSeedCapture(CAP);
		setCapture(CAP);
		toggleSpectrumZoom();
		expect((document.getElementById('btn-zoom-reset') as HTMLButtonElement).disabled).toBe(true);
		// (Becoming zoomed is the gesture layer's job; the render-integration tests cover it.)
	});

	it('PNM disables the button and hides the overview, without clearing the toggle', () => {
		toggleSpectrumZoom();              // On
		expect(isSpectrumZoomOn()).toBe(true);
		deviceMode.confirm('pnm');
		expect(spectrumZoomAvailable()).toBe(false);
		syncSpectrumZoomUi(true);
		expect(btn().disabled).toBe(true);
		expect(box().style.display).toBe('none');
		// Leaving PNM re-enables; the user's On choice survives but nothing re-magnifies.
		deviceMode.confirm('std');
		syncSpectrumZoomUi(true);
		expect(btn().disabled).toBe(false);
		expect(isSpectrumZoomOn()).toBe(true);
	});

	it('the title shows the captured range once a frame exists', () => {
		__testSeedCapture(CAP);
		setCapture(CAP);
		toggleSpectrumZoom();
		syncSpectrumZoomUi(true);
		const titleText = document.getElementById('zoom-overview-title')!.textContent!;
		expect(titleText).toContain('900');
		expect(titleText).toContain('GHz');
	});

	it('updates the overview ranges while an axis drag is held', () => {
		graphMode.confirm('std');
		S.setFreqArray(Float64Array.from([CAP.lo, CAP.hi]));
		__testSeedCapture(CAP);
		setView({ lo: 940e6, hi: 960e6 }, 1);
		zoomEnabled.set(true);
		syncSpectrumZoomUi(true);
		const title = document.getElementById('zoom-overview-title')!;
		const before = title.textContent;
		axisDragStart('x', 400, 450, { x: 0, y: 0, w: 800, h: 400 });
		axisDragMove(600, 450);
		syncSpectrumZoomUi();
		expect(title.textContent).toContain('935');
		expect(title.textContent).toContain('955');
		expect(title.textContent).toContain('895');
		resetAxisPreview();
		syncSpectrumZoomUi();
		expect(title.textContent).toBe(before);
	});


	describe('mode-STATUS lifecycle (the first STATUS is not a mode switch)', () => {
		const CAP2 = { lo: 900e6, hi: 1000e6 };

		it('keeps the capture when the first STATUS confirms the initial mode', () => {
			__testSeedCapture(CAP2);
			syncGraphModeStatus('std');     // first confirmation: nothing switched
			expect(sameWindow(getCapture(), CAP2)).toBe(true);
		});

		it('drops the capture on a real confirmed mode change', () => {
			syncGraphModeStatus('std');     // initial confirm
			__testSeedCapture(CAP2);
			syncGraphModeStatus('rta');     // a real switch: the view dies with the old axis
			expect(getCapture()).toBeNull();
		});
	});
});

describe('zoom rail display controls', () => {
	function slider(key: string): HTMLInputElement {
		return document.getElementById(`zoom-rail-${key}`) as HTMLInputElement;
	}
	function move(key: string, value: number): void {
		const input = slider(key);
		input.value = String(value);
		input.dispatchEvent(new Event('input', { bubbles: true }));
	}
	beforeEach(() => {
		clearDisplayBounds();
		S.setFreqArray(Float64Array.from({ length: 101 }, (_, i) => CAP.lo + i * 1e6));
		buildDom();
		document.body.insertAdjacentHTML('beforeend', '<div id="control-rail"><div id="rail-items"></div></div><div class="control-panel"><div class="control-group"><span class="group-title" data-i18n="graph">Graph</span></div></div>');
		initControlRail();
		initSpectrumZoomUi();
		vi.mocked(send).mockClear();
	});
	afterEach(() => { S.setFreqArray(null); });
	it('disables sliders without data and enables horizontal zoom at the full window', () => {
		toggleSpectrumZoom();
		for (const key of ['horizontal', 'top', 'bottom']) expect(slider(key).disabled).toBe(true);
		__testSeedCapture(CAP);
		syncSpectrumZoomUi(true);
		for (const key of ['horizontal', 'top', 'bottom']) expect(slider(key).disabled).toBe(false);
		expect(Number(slider('horizontal').value)).toBe(0);
		expect(slider('horizontal').getAttribute('aria-valuetext')).toBe('1× · 100.000 MHz');
	});
	it('zooms logarithmically from full capture to the native minimum span and back', () => {
		__testSeedCapture(CAP);
		toggleSpectrumZoom();
		move('horizontal', 500);
		const halfway = getView()!;
		expect(halfway.hi - halfway.lo).toBeCloseTo(100e6 / Math.sqrt(50), 3);
		expect((halfway.lo + halfway.hi) / 2).toBe(950e6);
		move('horizontal', 1000);
		expect(getView()).toEqual({ lo: 949e6, hi: 951e6 });
		expect(slider('horizontal').getAttribute('aria-valuetext')).toBe('50× · 2.000 MHz');
		expect(document.getElementById('zoom-rail-horizontal-value')!.textContent).toBe('x50');
		move('horizontal', 0);
		expect(getView()).toEqual(CAP);
		expect(slider('horizontal').disabled).toBe(false);
		expect(send).not.toHaveBeenCalled();
	});
	it('anchors zoom to the current view centre and clamps zoom-out at capture edges', () => {
		__testSeedCapture(CAP);
		toggleSpectrumZoom();
		setView({ lo: 910e6, hi: 930e6 }, 2e6);
		syncSpectrumZoomUi();
		move('horizontal', 1000);
		expect(getView()).toEqual({ lo: 919e6, hi: 921e6 });
		move('horizontal', 0);
		expect(getView()).toEqual(CAP);
	});
	it('reflects marquee and overview resizing while overview panning keeps the zoom', () => {
		__testSeedCapture(CAP);
		toggleSpectrumZoom();
		const plotWidth = S.W - S.MARGIN.left - S.MARGIN.right;
		expect(commitMarquee(S.MARGIN.left + plotWidth * 0.25, S.MARGIN.left + plotWidth * 0.75, 2e6)).toBe(true);
		syncSpectrumZoomUi();
		expect(getView()).toEqual({ lo: 925e6, hi: 975e6 });
		expect(Number(slider('horizontal').value)).toBe(Math.round(Math.log(2) / Math.log(50) * 1000));
		const overview = document.getElementById('zoom-overview') as HTMLCanvasElement;
		vi.spyOn(overview, 'getBoundingClientRect').mockReturnValue({ left: 0, width: 400 } as DOMRect);
		const pointer = (type: string, clientX: number) => overview.dispatchEvent(new MouseEvent(type, { clientX, bubbles: true }));
		pointer('pointerdown', 300); // Resize the upper edge from 975 MHz to 965 MHz.
		pointer('pointermove', 260);
		pointer('pointerup', 260);
		expect(getView()).toEqual({ lo: 925e6, hi: 965e6 });
		expect(Number(slider('horizontal').value)).toBe(Math.round(Math.log(2.5) / Math.log(50) * 1000));
		const zoomValue = slider('horizontal').value;
		pointer('pointerdown', 180); // Pan the 40 MHz view by 10 MHz.
		pointer('pointermove', 220);
		pointer('pointerup', 220);
		expect(getView()).toEqual({ lo: 935e6, hi: 975e6 });
		expect(slider('horizontal').value).toBe(zoomValue);
		expect(send).not.toHaveBeenCalled();
	});
	it('keeps reference and gain intent unchanged, survives ACK, and restores both axes', () => {
		__testSeedCapture(CAP);
		toggleSpectrumZoom();
		const before = [getDisplayRef(), refLevel.get(), S.dbPerDiv, ampAtten.get(), ampPreamp.get()];
		move('top', -20);
		move('bottom', -120);
		expect(getDisplayTop()).toBe(-20);
		expect(getDisplayRange()).toBe(100);
		noteDisplayRefReport(0);
		expect(getDisplayTop()).toBe(-20);
		expect([getDisplayRef(), refLevel.get(), S.dbPerDiv, ampAtten.get(), ampPreamp.get()]).toEqual(before);
		expect(send).not.toHaveBeenCalled();
		move('bottom', -20);
		expect(getDisplayRange()).toBeGreaterThanOrEqual(1);
		document.getElementById('btn-zoom-reset')!.click();
		expect(hasDisplayBounds()).toBe(false);
		move('top', -20);
		toggleSpectrumZoom();
		expect(hasDisplayBounds()).toBe(false);
		expect(getDisplayTop()).toBe(before[0]);
	});
	it('shows a switch only while zoom is on and toggles the group buttons against the sliders', () => {
		const button = document.getElementById('zoom-rail-switch') as HTMLButtonElement;
		const item = document.querySelector('.rail-item') as HTMLButtonElement;
		const sliders = document.getElementById('zoom-rail-controls') as HTMLElement;
		expect(button.hidden).toBe(true);
		expect(item.hidden).toBe(false);
		toggleSpectrumZoom();
		expect(button.hidden).toBe(false);
		expect(button.getAttribute('aria-pressed')).toBe('false');
		expect(item.hidden).toBe(true);
		expect(sliders.classList.contains('showing-groups')).toBe(false);
		button.click();
		expect(button.getAttribute('aria-pressed')).toBe('true');
		expect(item.hidden).toBe(false);
		expect(sliders.hidden).toBe(false);
		expect(sliders.classList.contains('showing-groups')).toBe(true);
		button.click();
		expect(item.hidden).toBe(true);
		expect(sliders.classList.contains('showing-groups')).toBe(false);
		toggleSpectrumZoom();
		expect(button.hidden).toBe(true);
		expect(item.hidden).toBe(false);
		expect(sliders.hidden).toBe(true);
		toggleSpectrumZoom();
		expect(button.getAttribute('aria-pressed')).toBe('false');
		expect(item.hidden).toBe(true);
	});
});
