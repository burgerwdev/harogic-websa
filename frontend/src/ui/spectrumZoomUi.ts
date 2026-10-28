/**
 * Top-bar zoom toggle + the global overview widget (display-only local zoom).
 *
 * Owns the DOM state machine of the design (~/harogic-websa-spectrum-local-zoom-design.md §3):
 *
 *   Off          - button idle, overview hidden (the default; a reload always lands here)
 *   On/waiting   - button pressed, overview visible, "waiting for spectrum data"
 *   On/full      - button pressed, overview visible, restore disabled (view == capture)
 *   On/zoomed    - button pressed, overview visible, restore enabled, view range in the title
 *   PNM          - button disabled + forced Off (the phase-noise plot has no linear
 *                  spectrum axis; leaving PNM re-enables but does not re-switch on)
 *
 * The toggle shares the keypad button's visual convention (`.active` + `aria-pressed`,
 * see ui/keypad.ts:syncToggle) but is otherwise independent: no shared localStorage, no
 * shared lifecycle. This module never sends anything - it only flips display state and
 * asks for a repaint. Gesture handling lives with the canvas (ui/controls.ts).
 */
import * as S from '../core/store';
import { getLang, t } from '../core/i18n';
import { formatBWHz, formatFreqHz } from '../core/fmt';
import { requestRender } from '../render/redraw';
import { deviceMode } from './graphMode';
import { onLangChange } from '../core/i18n';
import { fmtAxisLevel } from '../core/level';
import { displayUnit } from './displayState';
import { clearDisplayBounds, displayBoundsMax, displayBoundsMin, getDisplayRange, getDisplayTop, hasDisplayBounds, setDisplayBounds } from './displayRef';
import { syncControlRailZoom } from './controlRail';
import { cancelLevelPreview } from './axisDrag';
import {
	DETAIL_HINT_SAMPLES,
	isZoomed,
	getCapture,
	getView,
	minSpanHz,
	samplesInView,
	setView,
	clampView,
	panToCenter,
	viewportVersion,
	zoomAround,
	zoomEnabled,
	passbandViewActive,
	type FreqWindow,
} from './spectrumViewport';

let lastSyncKey = '';
let lastPassbandButton: HTMLButtonElement | null = null;
let lastPassbandButtonKey = '';

function railInput(key: string): HTMLInputElement | null {
	return document.getElementById(`zoom-rail-${key}`) as HTMLInputElement | null;
}

function syncZoomRail(on: boolean): void {
	syncControlRailZoom(on);
	const capture = getCapture(), view = getView();
	const ready = on && !!capture && !!view;
	const horizontal = railInput('horizontal');
	if (horizontal) {
		const span = view ? view.hi - view.lo : 0;
		const fullSpan = capture ? capture.hi - capture.lo : 0;
		const maxZoom = fullSpan ? Math.max(1, fullSpan / minSpanHz(S.freqArray)) : 1;
		const zoom = span ? fullSpan / span : 1;
		horizontal.min = '0'; horizontal.max = '1000'; horizontal.step = '1';
		horizontal.disabled = !ready;
		horizontal.value = String(maxZoom > 1 ? Math.round(Math.log(zoom) / Math.log(maxZoom) * 1000) : 0);
		const text = view ? `${Number(zoom.toPrecision(3))}× · ${formatBWHz(span)}` : t('zoom_waiting');
		horizontal.setAttribute('aria-valuetext', text);
		const output = document.getElementById('zoom-rail-horizontal-value');
		if (output) { output.textContent = view ? text : '—'; output.title = text; }
	}
	const top = getDisplayTop(), bottom = top - getDisplayRange();
	const minimum = Math.min(displayBoundsMin(), bottom), maximum = Math.max(displayBoundsMax(), top);
	for (const key of ['top', 'bottom']) {
		const input = railInput(key);
		if (!input) continue;
		const value = key === 'top' ? top : bottom;
		input.disabled = !ready;
		input.min = String(key === 'top' ? bottom + 1 : minimum);
		input.max = String(key === 'bottom' ? top - 1 : maximum);
		input.step = 'any';
		input.value = String(value);
		const text = `${fmtAxisLevel(value)} ${displayUnit.get() === 'dB' ? 'dB' : S.levelUnit}`;
		input.setAttribute('aria-valuetext', text);
		const output = document.getElementById(`zoom-rail-${key}-value`);
		if (output) { output.textContent = fmtAxisLevel(value); output.title = text; }
	}
}

function initZoomRail(): void {
	for (const key of ['horizontal', 'top', 'bottom']) {
		const input = railInput(key);
		input?.addEventListener('keydown', (event) => { event.stopPropagation(); });
		input?.addEventListener('input', () => {
			if (!isSpectrumZoomOn() || !getCapture() || !getView() || input.disabled) return;
			const value = Number(input.value);
			if (!Number.isFinite(value)) return;
			if (key === 'horizontal') {
				const capture = getCapture()!, view = getView()!;
				const fullSpan = capture.hi - capture.lo;
				const min = Math.min(fullSpan, minSpanHz(S.freqArray));
				// Equal slider steps multiply zoom; the current centre stays fixed until an edge clamps it.
				const span = fullSpan / Math.pow(fullSpan / min, value / 1000);
				setView(zoomAround(view, span / (view.hi - view.lo), (view.lo + view.hi) / 2, capture, min), min);
			} else {
				cancelLevelPreview();
				const top = getDisplayTop(), bottom = top - getDisplayRange();
				setDisplayBounds(key === 'top' ? Math.max(bottom + 1, value) : top,
					key === 'bottom' ? Math.min(top - 1, value) : bottom);
			}
			syncSpectrumZoomUi(true);
			requestRender();
		});
	}
}

/** Keep the filter action's fixed-width label while exposing toggle state accessibly. */
function syncPassbandViewButton(active: boolean): void {
	const button = document.getElementById('btn-sdr-filter-view') as HTMLButtonElement | null;
	if (!button) return;
	const key = `${active ? 1 : 0}|${button.disabled ? 1 : 0}|${getLang()}`;
	if (button === lastPassbandButton && key === lastPassbandButtonKey) return;
	lastPassbandButton = button;
	lastPassbandButtonKey = key;
	if (active) button.disabled = false; // Restore must remain available for an invalid new band.
	button.classList.toggle('active', active);
	button.setAttribute('aria-pressed', String(active));
	button.setAttribute('aria-label', t(active ? 'sdr_filter_restore' : 'sdr_filter_view_label'));
	button.dataset.i18n = 'sdr_filter_view';
	button.dataset.i18nTitle = active ? 'sdr_filter_restore_tip'
		: button.disabled ? 'sdr_filter_view_unavailable' : 'sdr_filter_view_tip';
	button.textContent = t('sdr_filter_view');
	button.title = t(button.dataset.i18nTitle);
}

/** PNM owns the canvas with a log-offset curve; zoom is meaningless there (design §2). */
export function spectrumZoomAvailable(): boolean {
	if (deviceMode.get() === 'pnm') return false;
	return !(S.measOn && S.viewMode === 'pnm');
}

/** True while the top-bar toggle is on and the mode allows the zoom UI at all. */
export function isSpectrumZoomOn(): boolean {
	return zoomEnabled.get() && spectrumZoomAvailable();
}

/** The overview box + title/hint follow the toggle, the view and the mode. */
export function syncSpectrumZoomUi(force = false): void {
	syncPassbandViewButton(passbandViewActive());
	const btn = document.getElementById('btn-spectrum-zoom') as HTMLButtonElement | null;
	const box = document.getElementById('zoom-overview-box');
	const reset = document.getElementById('btn-zoom-reset') as HTMLButtonElement | null;
	const title = document.getElementById('zoom-overview-title');
	const hint = document.getElementById('zoom-overview-hint');
	if (!btn || !box) return;

	const available = spectrumZoomAvailable();
	const on = zoomEnabled.get() && available;
	const zoomed = on && isZoomed();

	// One cheap string key guards the DOM writes: the render loop calls this every pass.
	const capture = getCapture();
	const view = getView();
	const samples = on && capture && view ? samplesInView(S.freqArray, view) : null;
	const key = [
		on ? 1 : 0, available ? 1 : 0, zoomed ? 1 : 0,
		viewportVersion(), S.freqVersion, capture ? capture.lo.toFixed(0) : '-',
		samples === null ? '-' : samples, getDisplayTop(), getDisplayRange(), getLang(), displayUnit.get(), S.levelUnit,
	].join('|');
	if (!force && key === lastSyncKey) return;
	lastSyncKey = key;
	syncZoomRail(on);

	btn.disabled = !available;
	btn.classList.toggle('active', on);
	btn.setAttribute('aria-pressed', String(on));
	btn.title = available ? t('tip_spectrum_zoom') : t('tip_spectrum_zoom') + ' (PNM)';
	box.style.display = on ? '' : 'none';
	if (reset) reset.disabled = !zoomed && !hasDisplayBounds();

	if (title) {
		if (!capture) {
			title.textContent = `${t('zoom_overview')} - ${t('zoom_waiting')}`;
		} else if (view && zoomed) {
			title.textContent = `${t('zoom_view_label')} ${formatFreqHz(view.lo)} - ${formatFreqHz(view.hi)}` +
				` / ${t('zoom_full_label')} ${formatFreqHz(capture.lo)} - ${formatFreqHz(capture.hi)}`;
		} else {
			title.textContent = `${t('zoom_overview')} ${formatFreqHz(capture.lo)} - ${formatFreqHz(capture.hi)}`;
		}
	}
	// Slider semantics for the focusable overview canvas: the value is the view centre.
	const ov = document.getElementById('zoom-overview');
	if (ov && capture && view && zoomed) {
		ov.setAttribute('aria-valuemin', String(capture.lo));
		ov.setAttribute('aria-valuemax', String(capture.hi));
		ov.setAttribute('aria-valuenow', String((view.lo + view.hi) / 2));
		ov.setAttribute('aria-valuetext', title?.textContent ?? '');
	}
	if (hint) {
		if (zoomed && samples !== null && samples < DETAIL_HINT_SAMPLES) {
			hint.textContent = t('zoom_sample_hint', { n: String(samples) });
			hint.style.display = '';
		} else {
			hint.style.display = 'none';
		}
	}
}

/** Toggle the zoom (the top-bar button; keyboard reaches it through normal focus). */
export function toggleSpectrumZoom(): void {
	if (!spectrumZoomAvailable()) return;
	const next = !zoomEnabled.get();
	zoomEnabled.set(next);
	// Off always returns to the full window (design §3); On keeps whatever comes next.
	if (!next) { setView(null); clearDisplayBounds(); }
	syncSpectrumZoomUi(true);
	requestRender();
}

/** The overview "restore" button: back to On/full, the toggle stays on. */
export function resetSpectrumZoom(): void {
	if (!isSpectrumZoomOn()) return;
	setView(null);
	clearDisplayBounds();
	syncSpectrumZoomUi(true);
	requestRender();
}

export function initSpectrumZoomUi(): void {
	document.getElementById('btn-spectrum-zoom')
		?.addEventListener('click', (e) => { e.stopPropagation(); toggleSpectrumZoom(); });
	document.getElementById('btn-zoom-reset')
		?.addEventListener('click', (e) => { e.stopPropagation(); resetSpectrumZoom(); });
		document.getElementById('btn-zoom-close')
		?.addEventListener('click', (e) => { e.stopPropagation(); if (isSpectrumZoomOn()) toggleSpectrumZoom(); });
	initOverviewInteractions();
	initZoomRail();
	onLangChange(() => syncSpectrumZoomUi(true));
	syncSpectrumZoomUi(true);
}

// ---------------- overview box interactions (display-only panning/resize) ----------------

/** Half of the touch target around a highlight edge (design §3: 24 CSS px total). */
const EDGE_HIT_PX = 12;

type OverviewDrag = { kind: 'pan' | 'lo' | 'hi'; lastHz: number } | null;
let ovDrag: OverviewDrag = null;

/** Frequency under a pointer x in the overview box (its full width is the capture). */
function overviewFreqAtX(canvas: HTMLCanvasElement, clientX: number): number | null {
	const capture = getCapture();
	if (!capture) return null;
	const rect = canvas.getBoundingClientRect();
	const frac = (clientX - rect.left) / Math.max(1, rect.width);
	return capture.lo + Math.min(1, Math.max(0, frac)) * (capture.hi - capture.lo);
}

function overviewEdgeHit(canvas: HTMLCanvasElement, clientX: number, view: FreqWindow,
	capture: FreqWindow): 'lo' | 'hi' | null {
	const rect = canvas.getBoundingClientRect();
	const px = (f: number) => (f - capture.lo) / (capture.hi - capture.lo) * rect.width;
	const x = clientX - rect.left;
	if (Math.abs(x - px(view.lo)) <= EDGE_HIT_PX) return 'lo';
	if (Math.abs(x - px(view.hi)) <= EDGE_HIT_PX) return 'hi';
	return null;
}

function applyOverviewDrag(canvas: HTMLCanvasElement, clientX: number): void {
	const capture = getCapture();
	const view = getView();
	if (!capture || !view || !ovDrag) return;
	const f = overviewFreqAtX(canvas, clientX);
	if (f === null) return;
	const min = minSpanHz(S.freqArray);
	if (ovDrag.kind === 'pan') {
		const center = (view.lo + view.hi) / 2 + (f - ovDrag.lastHz);
		setView(panToCenter(view, center, capture, min), min);
	} else if (ovDrag.kind === 'lo') {
		setView(clampView({ lo: f, hi: view.hi }, capture, min), min);
	} else {
		setView(clampView({ lo: view.lo, hi: f }, capture, min), min);
	}
	ovDrag.lastHz = f;
	syncSpectrumZoomUi();
	requestRender();
}

function initOverviewInteractions(): void {
	const canvas = document.getElementById('zoom-overview') as HTMLCanvasElement | null;
	if (!canvas) return;
	canvas.addEventListener('pointerdown', (e) => {
		const capture = getCapture();
		const view = getView();
		if (!isSpectrumZoomOn() || !capture || !view) return;
		const min = minSpanHz(S.freqArray);
		const edge = overviewEdgeHit(canvas, e.clientX, view, capture);
		if (edge) {
			ovDrag = { kind: edge, lastHz: overviewFreqAtX(canvas, e.clientX) ?? view.lo };
		} else {
			const f = overviewFreqAtX(canvas, e.clientX);
			if (f === null) return;
			if (f < view.lo || f > view.hi) {
				// A click outside the highlight recentres on that frequency, width kept.
				setView(panToCenter(view, f, capture, min), min);
				syncSpectrumZoomUi();
				requestRender();
				return;
			}
			ovDrag = { kind: 'pan', lastHz: f };
		}
		canvas.style.cursor = ovDrag.kind === 'pan' ? 'grabbing' : 'ew-resize';
		try { canvas.setPointerCapture(e.pointerId); } catch { /* jsdom/tests */ }
		e.preventDefault();
	});
	canvas.addEventListener('pointermove', (e) => {
		if (ovDrag) { applyOverviewDrag(canvas, e.clientX); return; }
		// Hover feedback only: edge vs pan vs jump-to.
		const capture = getCapture();
		const view = getView();
		if (!capture || !view) return;
		const edge = overviewEdgeHit(canvas, e.clientX, view, capture);
		canvas.style.cursor = edge ? 'ew-resize' : 'grab';
	});
	const endDrag = (e: PointerEvent) => {
		if (!ovDrag) return;
		ovDrag = null;
		canvas.style.cursor = 'grab';
		try { canvas.releasePointerCapture(e.pointerId); } catch { /* already released */ }
	};
	canvas.addEventListener('pointerup', endDrag);
	canvas.addEventListener('pointercancel', endDrag);
	canvas.addEventListener('blur', () => { ovDrag = null; });
	// Keyboard operation of the highlighted window (design §3). Only while focused, so
	// the SDR arrow-key tuning and the input fields are never shadowed.
	canvas.addEventListener('keydown', (e) => {
		const capture = getCapture();
		const view = getView();
		if (!capture || !view) return;
		const min = minSpanHz(S.freqArray);
		const step = (e.shiftKey ? 0.01 : 0.1) * (view.hi - view.lo);
		const center = (view.lo + view.hi) / 2;
		let handled = true;
		if (e.key === 'ArrowLeft') setView(panToCenter(view, center - step, capture, min), min);
		else if (e.key === 'ArrowRight') setView(panToCenter(view, center + step, capture, min), min);
		else if (e.key === '+' || e.key === '=') setView(zoomAround(view, 0.8, center, capture, min), min);
		else if (e.key === '-' || e.key === '_') setView(zoomAround(view, 1.25, center, capture, min), min);
		else if (e.key === 'Home') setView(null);
		else handled = false;
		if (handled) {
			e.preventDefault();
			e.stopPropagation();
			syncSpectrumZoomUi();
			requestRender();
		}
	});
}
