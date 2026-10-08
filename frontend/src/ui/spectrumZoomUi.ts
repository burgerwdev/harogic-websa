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
import { t } from '../core/i18n';
import { formatFreqHz } from '../core/fmt';
import { requestRender } from '../render/redraw';
import { deviceMode } from './graphMode';
import {
	DETAIL_HINT_SAMPLES,
	isZoomed,
	getCapture,
	getView,
	samplesInView,
	setView,
	viewportVersion,
	zoomEnabled,
} from './spectrumViewport';

let lastSyncKey = '';

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
		viewportVersion(), capture ? capture.lo.toFixed(0) : '-',
		samples === null ? '-' : samples,
	].join('|');
	if (!force && key === lastSyncKey) return;
	lastSyncKey = key;

	btn.disabled = !available;
	btn.classList.toggle('active', on);
	btn.setAttribute('aria-pressed', String(on));
	btn.title = available ? t('tip_spectrum_zoom') : t('tip_spectrum_zoom') + ' (PNM)';
	box.style.display = on ? '' : 'none';
	if (reset) reset.disabled = !zoomed;

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
	if (!next) setView(null);
	syncSpectrumZoomUi(true);
	requestRender();
}

/** The overview "restore" button: back to On/full, the toggle stays on. */
export function resetSpectrumZoom(): void {
	if (!isSpectrumZoomOn()) return;
	setView(null);
	syncSpectrumZoomUi(true);
	requestRender();
}

export function initSpectrumZoomUi(): void {
	document.getElementById('btn-spectrum-zoom')
		?.addEventListener('click', (e) => { e.stopPropagation(); toggleSpectrumZoom(); });
	document.getElementById('btn-zoom-reset')
		?.addEventListener('click', (e) => { e.stopPropagation(); resetSpectrumZoom(); });
	syncSpectrumZoomUi(true);
}
