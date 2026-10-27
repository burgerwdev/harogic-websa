import * as S from '../core/store';
import { plotRect } from '../render/plot';
import { previewFreqWindow } from './axisDrag';
import { sdrDemod, sdrListenHz, sdrPitchHz } from './sdrState';
import { getView, isZoomed, zoomEnabled, type FreqWindow } from './spectrumViewport';
import { requestRender } from '../render/redraw';

export const SDR_BADGE_FONT = '11px monospace';

/** The RTA frame, axis preview and local zoom must agree for drawing and hit testing. */
export function sdrSpectrumWindow(): FreqWindow | null {
  const d = S.rtaData;
  if (!d?.freq || d.freq.length < 2) return null;
  const preview = previewFreqWindow();
  const view = isZoomed() ? getView() : null;
  const window = preview ?? (view && view.lo >= d.startHz - 0.5 && view.hi <= d.stopHz + 0.5
    ? view : { lo: d.startHz, hi: d.stopHz });
  return Number.isFinite(window.lo) && Number.isFinite(window.hi) && window.hi > window.lo ? window : null;
}

export interface SdrBadgeGeometry {
  left: number; top: number; width: number; height: number; label: string;
  frequency: number; dialX: number; view: FreqWindow;
}

interface BadgeDrag {
  x: number; frequency: number; target: number; view: FreqWindow;
  width: number; height: number; ratio: number; captureLo: number; captureHi: number;
  zoomEpoch: number; demod: string; pitch: number;
  minimum: number; maximum: number; listen: number; connected: boolean; cancel: () => void;
}
let drag: BadgeDrag | null = null;
let hovered = false;

export function setSdrBadgeHover(active: boolean): void {
  if (hovered === active) return;
  hovered = active;
  requestRender();
}

export function sdrBadgeActive(): boolean {
  return hovered || drag !== null;
}

/** Cancel even without pointer movement when a redraw observes a changed environment. */
export function validateSdrBadgeDrag(): boolean {
  if (!drag) return false;
  const view = sdrSpectrumWindow();
  if (!S.sdrMode || S.deviceConnected !== drag.connected || !document.getElementById('spectrum') || !view
      || view.lo !== drag.view.lo || view.hi !== drag.view.hi
      || S.W !== drag.width || S.H !== drag.height || S.pixelRatio !== drag.ratio
      || S.rtaData.startHz !== drag.captureLo || S.rtaData.stopHz !== drag.captureHi
      || zoomEnabled.epoch !== drag.zoomEpoch
      || sdrDemod.get() !== drag.demod || sdrPitchHz.get() !== drag.pitch
      || S.FREQ_MIN !== drag.minimum || S.FREQ_MAX !== drag.maximum
      || sdrListenHz.get() !== drag.listen) {
    const cancel = drag.cancel;
    drag = null;
    cancel();
    return false;
  }
  return true;
}

export function sdrBadgeDragFrequency(): number | null {
  return validateSdrBadgeDrag() ? drag!.target : null;
}

/** Measure once with the drawing font; both the renderer and controls use this rectangle. */
export function sdrBadgeGeometry(): SdrBadgeGeometry | null {
  const view = sdrSpectrumWindow();
  const frequency = sdrBadgeDragFrequency() ?? sdrListenHz.get();
  if (!S.sdrMode || !view || !S.ctx || !(frequency > 0) || frequency < view.lo || frequency > view.hi) return null;
  const plot = plotRect();
  if (!(plot.w > 0)) return null;
  const label = Math.round(frequency).toLocaleString('en-US') + ' Hz';
  S.ctx.save();
  S.ctx.font = SDR_BADGE_FONT;
  const width = Math.min(plot.w, S.ctx.measureText(label).width + 26);
  S.ctx.restore();
  const dialX = plot.x + (frequency - view.lo) / (view.hi - view.lo) * plot.w;
  const height = 18;
  return { left: Math.max(plot.x, Math.min(dialX - width / 2, plot.x + plot.w - width)),
    top: Math.max(0, plot.y - height - 6), width, height, label, frequency, dialX, view };
}

export function sdrBadgeAt(x: number, y: number): SdrBadgeGeometry | null {
  const g = sdrBadgeGeometry();
  return g && x >= g.left && x <= g.left + g.width && y >= Math.max(0, g.top - 4)
    && y <= g.top + g.height + 4 ? g : null;
}

export function startSdrBadgeDrag(x: number, g: SdrBadgeGeometry, cancel: () => void): void {
  drag = { x, frequency: g.frequency, target: g.frequency, view: g.view,
    width: S.W, height: S.H, ratio: S.pixelRatio, captureLo: S.rtaData.startHz, captureHi: S.rtaData.stopHz,
    zoomEpoch: zoomEnabled.epoch, demod: sdrDemod.get(), pitch: sdrPitchHz.get(),
    minimum: S.FREQ_MIN, maximum: S.FREQ_MAX, listen: sdrListenHz.get(), connected: S.deviceConnected, cancel };
}

export function moveSdrBadgeDrag(x: number): number | null {
  if (!validateSdrBadgeDrag()) return null;
  const d = drag!;
  const lo = Math.max(d.view.lo, S.FREQ_MIN), hi = Math.min(d.view.hi, S.FREQ_MAX);
  if (lo > hi) return null;
  d.target = Math.max(lo, Math.min(hi, d.frequency + (x - d.x) / plotRect().w * (d.view.hi - d.view.lo)));
  return d.target;
}

export function clearSdrBadgeDrag(): void {
  drag = null;
}
