/**
 * The top-bar zoom toggle + overview widget state machine (ui/spectrumZoomUi.ts).
 *
 * Pins the design §3 contract: the button mirrors the keypad button's convention
 * (.active + aria-pressed) but nothing else; the overview box follows the toggle;
 * Off returns to the full window; PNM disables the whole UI without touching the
 * user's toggle choice; none of it sends anything.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import { deviceMode } from '../ui/graphMode';
import { setCapture, zoomEnabled, __testSeedCapture } from '../ui/spectrumViewport';
import {
	initSpectrumZoomUi,
	isSpectrumZoomOn,
	spectrumZoomAvailable,
	syncSpectrumZoomUi,
	toggleSpectrumZoom,
} from '../ui/spectrumZoomUi';

const CAP = { lo: 900e6, hi: 1000e6 };

function buildDom(): void {
	document.body.innerHTML = `
		<div class="info-right">
			<button class="btn bar-btn" id="btn-spectrum-zoom" aria-pressed="false"></button>
		</div>
		<div class="spectrum-area">
			<div id="zoom-overview-box" style="display:none;">
				<div class="zoom-overview-head">
					<span id="zoom-overview-title"></span>
					<button class="btn zoom-reset-btn" id="btn-zoom-reset" disabled></button>
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
		expect(document.getElementById('zoom-overview-title')!.textContent).toContain('');
	});

	it('Off returns to the full window (the view is dropped)', () => {
		__testSeedCapture(CAP);
		setCapture(CAP);
		toggleSpectrumZoom();              // On
		toggleSpectrumZoom();              // Off again
		expect(isSpectrumZoomOn()).toBe(false);
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
});
