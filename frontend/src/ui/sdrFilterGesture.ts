/** Filter gestures own one pointer and snapshot mode/listen/pitch. Moves preview only;
 * release returns a value for the command owner. Coordinates use logical canvas pixels. */
import { ctx } from '../core/store';
import * as S from '../core/store';
import { canvasColors } from '../core/theme';
import { drawPassband } from '../render/passband';
import { plotRect } from '../render/plot';
import { demodBandHz, sdrDemod, sdrIfbw, sdrListenHz, sdrPitchHz } from './sdrState';
import { isDigitalMode } from '../sdr/registry';
import {
	DETAIL_HINT_SAMPLES,
	getCapture,
	isZoomed,
	getView,
	minSpanHz,
	passbandViewActive,
	samplesInView,
	togglePassbandView,
} from './spectrumViewport';
import { zoomEnabled } from './spectrumViewport';
import { syncSpectrumZoomUi } from './spectrumZoomUi';
import { requestRender } from '../render/redraw';
import { filterPreviewHz, filterRange, filterSpec, setFilterPreview } from './sdrFilter';
import { formatFreqHz } from '../core/fmt';

/** Which band edge a gesture is on. Symmetric modes mirror both onto the same drag. */
export type FilterEdge = 'low' | 'high';

/** Edge the DSP drag writes, after the mode mapping. */
export type DragTarget = 'low' | 'high';

export interface FilterHandle {
  edge: FilterEdge;
  /** Arrow centre and the real cutoff, in logical canvas pixels. */
  x: number;
  y: number;
  edgeX: number;
}

/** Passband geometry in canvas logical px + Hz for hit tests and the renderer. */
export interface FilterGeometry {
  dialX: number;
  plot: { x: number; y: number; w: number; h: number };
  lowX: number | null;
  highX: number | null;
  lowHz: number;
  highHz: number;
  /** The only edges that may be hit/dragged in this mode. */
  candidates: FilterEdge[];
  handles: FilterHandle[];
  mode: string;
  band: [number, number];
}

/** Hit/drag candidates per mode; empty for non-editable (protocol) modes. */
function edgeCandidates(mode: string): FilterEdge[] {
  if (!filterSpec(mode).editable || isDigitalMode(mode)) return [];
  if (mode === 'usb') return ['high'];       // outer cutoff only; the guard is not a target
  if (mode === 'lsb') return ['low'];
  return ['low', 'high'];
}

/** Edge -> drag target (SSB folds its single outer edge onto the outer cutoff). */
function dragTargetFor(mode: string, edge: FilterEdge): DragTarget {
  if (mode === 'usb') return 'high';
  if (mode === 'lsb') return 'low';
  return edge;
}

/** Geometry in the current display window; null when the band is outside it. */
export function filterGeometry(ifBwOverride?: number, modeOverride?: string): FilterGeometry | null {
  if (!S.sdrMode) return null;
  const listen = sdrListenHz.get();
  if (!(listen > 0)) return null;
  const mode = modeOverride ?? sdrDemod.get();
  const ifBw = ifBwOverride ?? filterPreviewHz();
  const [bandLo, bandHi] = demodBandHz(mode, ifBw, sdrPitchHz.get());
  if (!(bandHi > bandLo)) return null;
  const view = isZoomed() ? getView() : getCapture();
  if (!view || !(view.hi > view.lo)) return null;
  const p = plotRect();
  const xAt = (f: number) => p.x + (f - view.lo) / (view.hi - view.lo) * p.w;
  const dialX = xAt(listen);
  const loX = xAt(listen + bandLo);
  const hiX = xAt(listen + bandHi);
  // The band overlaps the window when its Hz ranges intersect (a wide band whose three
  // feature points all sit outside the window still spans it); dial visibility is its
  // own question, so it never gates the geometry.
  if (listen + bandHi < view.lo || listen + bandLo > view.hi) return null;
  const candidates = edgeCandidates(mode);
  return {
    dialX,
    plot: { x: p.x, y: p.y, w: p.w, h: p.h },
    lowX: loX >= p.x && loX <= p.x + p.w ? loX : null,
    highX: hiX >= p.x && hiX <= p.x + p.w ? hiX : null,
    lowHz: listen + bandLo,
    highHz: listen + bandHi,
    candidates,
    handles: filterHandles(p, dialX, loX, hiX, candidates),
    mode,
    band: [bandLo, bandHi],
  };
}

export const FILTER_HANDLE_HEIGHT = 18;

/** Keep the actual band untouched while separating the small controls above it. */
function filterHandles(p: FilterGeometry['plot'], dialX: number, lowX: number, highX: number,
  candidates: FilterEdge[]): FilterHandle[] {
  candidates = candidates.filter(edge => {
    const x = edge === 'low' ? lowX : highX;
    return x >= p.x && x <= p.x + p.w;
  });
  const inset = Math.min(6, p.w / 2);
  const left = p.x + inset, right = p.x + p.w - inset;
  const clamp = (x: number) => Math.max(left, Math.min(right, x));
  let low = clamp(lowX), high = clamp(highX);
  if (candidates.length === 2) {
    const separation = Math.min(24, right - left);
    if (high - low < separation) {
      const middle = Math.max(left + separation / 2,
        Math.min(right - separation / 2, (lowX + highX) / 2));
      low = middle - separation / 2;
      high = middle + separation / 2;
    }
  } else if (candidates[0] === 'low') {
    low = clamp(Math.min(lowX, dialX - 12));
  } else if (candidates[0] === 'high') {
    high = clamp(Math.max(highX, dialX + 12));
  }
  return candidates.map(edge => ({ edge, x: edge === 'low' ? low : high,
    y: p.y + FILTER_HANDLE_HEIGHT / 2, edgeX: edge === 'low' ? lowX : highX }));
}
/** Top arrows own distinct nearest-edge targets; zoom keeps the plot below for selection. */
export function filterEdgeAt(x: number, y: number, tolerance: number): FilterEdge | null {
  const tol = Math.max(0, tolerance);
  const g = filterGeometry();
  if (!g || !g.handles.length) return null;
  if (y < g.plot.y || y > g.plot.y + g.plot.h) return null;
  if (x < g.plot.x || x > g.plot.x + g.plot.w) return null;
  const top = y <= g.plot.y + FILTER_HANDLE_HEIGHT;
  if (zoomEnabled.get() && !top) return null;
  let best: { edge: FilterEdge; dist: number } | null = null;
  for (const handle of g.handles) {
    // Offset controls are only targets where they are drawn, leaving the dial usable.
    if (!top && Math.abs(handle.x - handle.edgeX) > 0.5) continue;
    const dist = Math.abs(x - (top ? handle.x : handle.edgeX));
    if (dist <= tol && (!best || dist < best.dist)) best = { edge: handle.edge, dist };
  }
  return best ? best.edge : null;
}

/** Dial hit for tuning; edge hit zones exclude this region. */
export function filterCenterAt(x: number, y: number, tolerance: number): boolean {
  const g = filterGeometry();
  if (!g) return false;
  if (y < g.plot.y || y > g.plot.y + g.plot.h) return false;
  if (x < g.plot.x || x > g.plot.x + g.plot.w) return false;
  return Math.abs(x - g.dialX) <= Math.max(0, tolerance);
}

// ---------------- drag state machine ----------------

interface DragState {
  pointerId: number;
  semantic: DragTarget;
  mode: string;
  listen: number;
  pitch: number;
  startX: number;
  startBandwidth: number;
  hzPerPixel: number;
}

let drag: DragState | null = null;
let hover: FilterEdge | null = null;
let tuneDragging = false;

export function setPassbandTuneDragging(active: boolean): void {
  tuneDragging = active;
}

export function passbandDragFrequency(): number | null {
  return S.sdrMode && (tuneDragging || drag !== null) ? sdrListenHz.get() : null;
}

/** True while a filter drag owns its pointer. */
export function filterDragging(): boolean {
  return drag !== null;
}

/** Begin a drag; false (stays idle) when the mode exposes no edges. */
export function startFilterDrag(edge: FilterEdge, pointerId: number, startX: number): boolean {
  cancelFilterDrag();
  const g = filterGeometry();
  if (!g || !g.candidates.includes(edge)) return false;
  const view = isZoomed() ? getView() : getCapture();
  if (!view || !(g.plot.w > 0)) return false;
  drag = {
    pointerId,
    semantic: dragTargetFor(g.mode, edge),
    mode: g.mode,
    listen: sdrListenHz.get(),
    pitch: sdrPitchHz.get(),
    startX,
    startBandwidth: filterPreviewHz(),
    hzPerPixel: (view.hi - view.lo) / g.plot.w,
  };
  return true;
}

/** Foreign pointer IDs are ignored; changed receiver geometry cancels the drag. */
export function moveFilterDrag(x: number, pointerId: number): boolean {
  if (!drag || drag.pointerId !== pointerId) return false;
  if (drag.mode !== sdrDemod.get() || Math.abs(drag.listen - sdrListenHz.get()) > 0.5
      || Math.abs(drag.pitch - sdrPitchHz.get()) > 0.5) {
    cancelFilterDrag();
    return true;
  }
  const next = drag.startBandwidth + (x - drag.startX) * drag.hzPerPixel
    * (drag.semantic === 'low' ? -1 : 1) * (drag.mode === 'usb' || drag.mode === 'lsb' ? 1 : 2);
  const spec = filterSpec(drag.mode);
  if (!isFinite(next) || !spec.editable) return false;
  const range = filterRange(drag.mode);
  if (range.max === null || range.max < range.min) return false;
  const clamped = Math.min(range.max, Math.max(next, range.min));
  if (clamped === filterPreviewHz()) return false;
  setFilterPreview(clamped);
  requestRender();
  return true;
}

/** Return a changed preview, then clear drag/hover/preview; never send a command. */
export function endFilterDrag(pointerId: number): number | null {
  if (!drag || drag.pointerId !== pointerId) return null;
  // Snapshot guard even without a final move: a mode/listen/pitch drift since the last
  // checked move invalidates the gesture's premise, so it ends cancelled, not committed.
  if (drag.mode !== sdrDemod.get() || Math.abs(drag.listen - sdrListenHz.get()) > 0.5
      || Math.abs(drag.pitch - sdrPitchHz.get()) > 0.5) {
    cancelFilterDrag();
    return null;
  }
  const preview = filterPreviewHz();
  const value = preview !== sdrIfbw.get() ? preview : null;
  drag = null;
  hover = null;
  setFilterPreview(null);
  requestRender();
  return value;
}

/** Abandon the gesture and its preview (Esc, pointercancel, lostpointercapture, blur,
 * mode switch, Preset, disconnect). True when any state was dropped. */
export function cancelFilterDrag(): boolean {
  const hadDrag = drag !== null;
  // Clear even when the preview returned to the slot value.
  const hadPreview = filterPreviewHz() !== sdrIfbw.get();
  drag = null;
  hover = null;
  setFilterPreview(null);
  if (hadDrag || hadPreview) requestRender();
  return hadDrag || hadPreview;
}

/** Hover changes cannot replace an active drag. */
export function setFilterHover(edge: FilterEdge | null): void {
  if (drag || hover === edge) return;
  hover = edge;
  requestRender();
}

/** The hovered edge (renderer input); null while dragging or not hovering. */
export function filterHover(): FilterEdge | null {
  return drag ? null : hover;
}

// ---------------- "view passband" ----------------

/** Focus the passband, or restore the previous raw view and zoom preference on a second use. */
export function viewSdrPassband(): boolean {
  cancelFilterDrag();
  const minSpan = minSpanHz(S.freqArray);
  // Restoration remains available after a mode/filter edit makes the current band invalid.
  if (passbandViewActive()) {
    togglePassbandView(getView() ?? { lo: 0, hi: 1 }, minSpan);
    syncSpectrumZoomUi(true);
    requestRender();
    return true;
  }
  const capture = getCapture();
  if (!S.sdrMode || !(sdrListenHz.get() > 0) || !capture) return false;
  // From the SLOTS, not the on-screen geometry: the entry must still find the band
  // when the current view does not show it, and the target is the band the receiver
  // reads inside THIS capture (edges outside the capture clamp to its ends).
  const [bandLo, bandHi] = demodBandHz(sdrDemod.get(), filterPreviewHz(), sdrPitchHz.get());
  if (!(bandHi > bandLo)) return false;
  const listen = sdrListenHz.get();
  const lo = Math.max(listen + bandLo, capture.lo);
  const hi = Math.min(listen + bandHi, capture.hi);
  if (!(hi > lo)) return false;                 // band entirely outside the capture
  const pad = Math.max((hi - lo) * 0.25, minSpan);
  if (!togglePassbandView({ lo: lo - pad, hi: hi + pad }, minSpan)) return false;
  syncSpectrumZoomUi(true);
  requestRender();
  return true;
}

/** Real samples inside the current view + whether that is at the data's resolution
 * limit (the honesty readout for the "view passband" entry). */
export function filterViewResolution(): { samples: number; atDataLimit: boolean } | null {
  const view = getView();
  if (!view) return null;
  const samples = samplesInView(S.freqArray, view) ?? 0;
  return { samples, atDataLimit: samples <= DETAIL_HINT_SAMPLES };
}

// ---------------- renderer hook ----------------

/** Draw inside the caller's plot clip; a badge drag may preview a different dial frequency. */
export function renderFilterPassband(loHz: number, hiHz: number, listenPreview: number | null = null): boolean {
  if (!S.sdrMode) return false;
  const listen = listenPreview ?? sdrListenHz.get();
  if (!(listen > 0)) return false;
  const mode = sdrDemod.get();
  const [bandLo, bandHi] = demodBandHz(mode, filterPreviewHz(), sdrPitchHz.get());
  if (!(bandHi > bandLo)) return false;
  const p = plotRect();
  const span = (hiHz - loHz) || 1;
  const xAt = (f: number) => p.x + (f - loHz) / span * p.w;
  const dialX = xAt(listen);
  const x0 = xAt(listen + bandLo);
  const x1 = xAt(listen + bandHi);
  // Band∩window in Hz: a wide band whose feature points all sit outside the window
  // still spans it and must be drawn (the canvas clip handles the actual bounds).
  if (listen + bandHi < loHz || listen + bandLo > hiHz) return false;

  // Diagnostic/e2e contract: what the overlay claims, in Hz relative to the dial (the
  // pixel scan cannot read these edges off the canvas - see the old spectrum.ts note).
  const ds = `${bandLo}..${bandHi}`;
  if (ctx.canvas.dataset.demodBand !== ds) ctx.canvas.dataset.demodBand = ds;

  const colors = canvasColors();
  drawPassband(ctx, x0, x1, p.x, p.y, p.w, p.h, colors, false);
  ctx.strokeStyle = colors.rta;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.beginPath();
  ctx.moveTo(dialX, p.y);
  ctx.lineTo(dialX, p.y + p.h);
  ctx.stroke();
  ctx.setLineDash([]);

  const handles = filterHandles(p, dialX, x0, x1, edgeCandidates(mode));
  for (const handle of handles) {
    const { edge, x, y, edgeX } = handle;
    const active = drag?.semantic === edge || hover === edge;
    ctx.strokeStyle = colors.rta;
    ctx.lineWidth = 1;
    if (Math.abs(x - edgeX) > 0.5) {
      ctx.globalAlpha = active ? 0.8 : 0.4;
      ctx.beginPath();
      ctx.moveTo(x, y + 4);
      ctx.lineTo(Math.max(p.x, Math.min(p.x + p.w, edgeX)), y + 7);
      ctx.stroke();
    }
    // Fine outward chevrons, without plates or grip marks.
    ctx.globalAlpha = active ? 1 : 0.7;
    ctx.lineWidth = active ? 1.8 : 1.2;
    const direction = edge === 'low' ? -1 : 1;
    ctx.beginPath();
    ctx.moveTo(x - direction * 3.5, y - 3.5);
    ctx.lineTo(x + direction * 3.5, y);
    ctx.lineTo(x - direction * 3.5, y + 3.5);
    ctx.stroke();
    ctx.globalAlpha = 1;
    if (!active) continue;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(edgeX, p.y);
    ctx.lineTo(edgeX, p.y + p.h);
    ctx.stroke();
    const cutoff = edge === 'low' ? bandLo : bandHi;
    const label = `${cutoff > 0 ? '+' : ''}${formatFreqHz(cutoff)} · BW ${formatFreqHz(bandHi - bandLo)}`;
    ctx.font = '11px monospace';
    const width = ctx.measureText(label).width + 8;
    const left = Math.max(p.x, Math.min(x - width / 2, p.x + p.w - width));
    ctx.fillStyle = colors.bg;
    ctx.fillRect(left, p.y + FILTER_HANDLE_HEIGHT + 3, width, 16);
    ctx.fillStyle = colors.rta;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(label, left + 4, p.y + FILTER_HANDLE_HEIGHT + 11);
  }
  return true;
}
