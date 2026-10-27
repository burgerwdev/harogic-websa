/**
 * The top-bar zoom toggle + overview widget state machine (ui/spectrumZoomUi.ts).
 *
 * Pins the design §3 contract: the button mirrors the keypad button's convention
 * (.active + aria-pressed) but nothing else; the overview box follows the toggle;
 * Off returns to the full window; PNM disables the whole UI without touching the
 * user's toggle choice; none of it sends anything.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
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
import { clearDisplayBounds, getDisplayRange, getDisplayRef, getDisplayTop, hasDisplayBounds, noteDisplayRefReport } from '../ui/displayRef';
import { refLevel } from '../ui/refState';
import { ampAtten, ampPreamp } from '../ui/ampState';
import { send } from '../core/wsSend';
import type * as WsSendModule from '../core/wsSend';

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
		buildDom();
		document.body.insertAdjacentHTML('beforeend', '<div id="control-rail"><div id="rail-items"></div></div><div class="control-panel"></div>');
		initControlRail();
		initSpectrumZoomUi();
		vi.mocked(send).mockClear();
	});
	it('disables every slider without data, and position at the full window', () => {
		toggleSpectrumZoom();
		for (const key of ['position', 'top', 'bottom']) expect(slider(key).disabled).toBe(true);
		__testSeedCapture(CAP);
		syncSpectrumZoomUi(true);
		expect(slider('position').disabled).toBe(true);
		expect(slider('top').disabled).toBe(false);
	});
	it('moves the overview in both directions while preserving the selected span', () => {
		__testSeedCapture(CAP);
		toggleSpectrumZoom();
		setView({ lo: 940e6, hi: 960e6 }, 1);
		syncSpectrumZoomUi();
		expect(Number(slider('position').value)).toBe(500);
		move('position', 1000);
		expect(getView()).toEqual({ lo: 980e6, hi: 1000e6 });
		setView({ lo: 900e6, hi: 920e6 }, 1);
		syncSpectrumZoomUi();
		expect(Number(slider('position').value)).toBe(0);
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
});
