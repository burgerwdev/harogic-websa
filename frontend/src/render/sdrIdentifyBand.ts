import { canvasColors } from '../core/theme';
import { currentGraphMode } from '../ui/graphMode';
import { identifyBand, identifySelecting, selectionHint } from '../ui/sdrIdentifySelection';
export function renderIdentifyBand(ctx: CanvasRenderingContext2D, lo: number, hi: number,
  p: { x: number; y: number; w: number; h: number }, text = false): void {
  if (currentGraphMode() !== 'sdr' || !(hi > lo)) return;
  const band = identifyBand(), col = canvasColors();
  ctx.save(); ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  if (band) {
    const left = p.x + (band.low_hz - lo) / (hi - lo) * p.w, right = p.x + (band.high_hz - lo) / (hi - lo) * p.w;
    ctx.fillStyle = col.text; ctx.globalAlpha = 0.10; ctx.fillRect(left, p.y, Math.max(1, right - left), p.h);
    ctx.globalAlpha = 0.8; ctx.strokeStyle = col.text; ctx.lineWidth = 1;
    for (const x of [left, right]) { ctx.beginPath(); ctx.moveTo(x, p.y); ctx.lineTo(x, p.y + p.h); ctx.stroke(); }
  }
  if (text && identifySelecting()) {
    const hint = selectionHint(), frequency = band ? `${(band.low_hz / 1e6).toFixed(6)}–${(band.high_hz / 1e6).toFixed(6)} MHz · ${((band.high_hz - band.low_hz) / 1e3).toFixed(1)} kHz` : '';
    ctx.font = '11px monospace'; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
    const width = Math.max(0, Math.min(p.w - 12, Math.max(ctx.measureText(hint).width, ctx.measureText(frequency).width) + 12));
    ctx.globalAlpha = 0.38; ctx.fillStyle = col.bg; ctx.fillRect(p.x + 6, p.y + p.h - 37, width, 32);
    ctx.globalAlpha = 0.95; ctx.fillStyle = col.text;
    ctx.fillText(hint, p.x + 12, p.y + p.h - 34, Math.max(1, width - 12));
    if (band) ctx.fillText(frequency, p.x + 12, p.y + p.h - 19, Math.max(1, width - 12));
  }
  ctx.restore();
}
