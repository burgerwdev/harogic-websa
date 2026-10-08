/**
 * The global-overview widget renderer (display-only local zoom, design §3).
 *
 * Draws the CURRENT frame's full captured window with the active trace and the highlighted
 * view window on the small canvas between the plot and the waterfall split. It reads only
 * data the renderer already has (S.freqArray / trace displays / rtaData) — no extra state,
 * no backend traffic. The canvas is sized like the waterfall (CSS px box, DPR-scaled
 * backing store, layout reads only on the resize/version path, never per frame).
 */
import * as S from '../core/store';
import { onCanvasResize } from '../core/store';
import { canvasColors } from '../core/theme';
import { t } from '../core/i18n';
import { isSpectrumZoomOn } from '../ui/spectrumZoomUi';
import { getMarqueePreview } from '../ui/spectrumZoomGestures';
import {
	getCapture,
	getView,
	viewportVersion,
} from '../ui/spectrumViewport';

let sized = false;
let sizedVersion = -1;
let ovW = 0, ovH = 0, ovRatio = 1;

function sizeOverview(cv: HTMLCanvasElement, ver: number): boolean {
	const w = cv.clientWidth, h = cv.clientHeight;   // layout-read-ok: resize path only (sized flag + version gate), never per frame
	if (!w || !h) { sized = false; return false; }
	const ratio = Math.max(1, Math.min(4, window.devicePixelRatio || 1));
	const bw = Math.round(w * ratio), bh = Math.round(h * ratio);
	if (cv.width !== bw) cv.width = bw;
	if (cv.height !== bh) cv.height = bh;
	ovW = w; ovH = h; ovRatio = ratio;
	sized = true;
	sizedVersion = ver;
	return true;
}

/** Active-trace display levels for the overview sketch (null while nothing is drawn). */
function overviewLevels(): Float32Array | null {
	const idx = S.activeTraceIdx;
	if (S.rtaMode) {
		const d = S.rtaData;
		const disp = S.rtaDisplays[idx];
		if (disp && disp.length > 1) return disp;
		return d && d.spec && d.spec.length > 1 ? d.spec : null;
	}
	const p = S.traces[idx]?.powers;
	return p && p.length > 1 ? p : null;
}

/** One overview pass; a no-op unless the zoom UI is on and a capture window exists. */
export function renderSpectrumZoomOverview(): void {
	if (!isSpectrumZoomOn()) { sized = false; return; }
	const cv = document.getElementById('zoom-overview') as HTMLCanvasElement | null;
	if (!cv) return;
	const ver = viewportVersion();
	if (!sized || sizedVersion !== ver) {
		if (!sizeOverview(cv, ver)) return;
	}
	const ctx = cv.getContext('2d');
	if (!ctx) return;
	const cap = getCapture();
	ctx.setTransform(ovRatio, 0, 0, ovRatio, 0, 0);
	const col = canvasColors();
	ctx.clearRect(0, 0, ovW, ovH);
	ctx.fillStyle = col.bg;
	ctx.fillRect(0, 0, ovW, ovH);
	if (!cap) return;

	const powers = overviewLevels();
	const n = powers?.length ?? 0;
	if (!powers || n < 2) {
		ctx.fillStyle = col.axis;
		ctx.font = '10px monospace';
		ctx.textAlign = 'center';
		ctx.textBaseline = 'middle';
		ctx.fillText(t('zoom_waiting'), ovW / 2, ovH / 2);
		return;
	}

	// Level normalisation: the frame's own finite range, 10% headroom (the overview is a
	// shape sketch, not a measurement — the main plot keeps the real axis).
	let lo = Infinity, hi = -Infinity;
	for (let i = 0; i < n; i++) {
		const v = powers[i];
		if (Number.isFinite(v)) { if (v < lo) lo = v; if (v > hi) hi = v; }
	}
	if (!Number.isFinite(lo) || !Number.isFinite(hi) || hi - lo < 1) { lo = -100; hi = 0; }
	const pad = (hi - lo) * 0.1;
	const Y = (v: number) => (1 - (v - (lo - pad)) / (hi - lo + 2 * pad)) * ovH;
	const X = (i: number) => (i / (n - 1)) * ovW;

	// The trace across the FULL capture (the view highlight is what marks the sub-window).
	ctx.strokeStyle = col.traces[S.activeTraceIdx] || col.traces[0] || col.rta;
	ctx.lineWidth = 1;
	ctx.beginPath();
	let pen = false;
	for (let i = 0; i < n; i++) {
		const v = powers[i];
		if (!Number.isFinite(v)) { pen = false; continue; }
		const x = X(i), y = Y(v);
		if (!pen) { ctx.moveTo(x, y); pen = true; } else ctx.lineTo(x, y);
	}
	ctx.stroke();

	// View highlight: dim the outside, outline the window, edge handles on both sides.
	const view = getView();
	const marq = getMarqueePreview();
	const win = marq ?? (view && isSubwindow(view, cap) ? view : null);
	if (win) {
		const xa = (win.lo - cap.lo) / (cap.hi - cap.lo) * ovW;
		const xb = (win.hi - cap.lo) / (cap.hi - cap.lo) * ovW;
		ctx.fillStyle = 'rgba(0,0,0,0.35)';
		ctx.fillRect(0, 0, xa, ovH);
		ctx.fillRect(xb, 0, Math.max(0, ovW - xb), ovH);
		ctx.strokeStyle = col.text;
		ctx.lineWidth = 1;
		ctx.strokeRect(xa, 0.5, Math.max(1, xb - xa), ovH - 1);
		ctx.fillStyle = col.text;
		ctx.fillRect(xa - 1, 0, 2, ovH);
		ctx.fillRect(xb - 1, 0, 2, ovH);
	}
}

function isSubwindow(v: { lo: number; hi: number }, cap: { lo: number; hi: number }): boolean {
	return Math.abs(v.lo - cap.lo) > 0.5 || Math.abs(v.hi - cap.hi) > 0.5;
}

// A window resize changes the overview box too: drop the size cache, the next pass re-reads.
onCanvasResize(() => { sized = false; });
