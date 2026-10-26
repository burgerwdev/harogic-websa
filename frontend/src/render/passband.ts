import type { CanvasColors } from '../core/theme';

export function drawPassband(
  ctx: CanvasRenderingContext2D, x0: number, x1: number,
  left: number, top: number, width: number, height: number,
  colors: CanvasColors, edges: boolean,
): void {
  const lo = Math.max(left, x0), hi = Math.min(left + width, x1);
  if (!(hi > lo)) return;
  ctx.save();
  ctx.beginPath();
  ctx.rect(left, top, width, height);
  ctx.clip();
  ctx.fillStyle = colors.passband;
  ctx.fillRect(lo, top, hi - lo, height);
  if (!edges) { ctx.restore(); return; }
  ctx.setLineDash([]);
  ctx.beginPath();
  if (x0 >= left && x0 <= left + width) {
    ctx.moveTo(x0, top); ctx.lineTo(x0, top + height);
  }
  if (x1 >= left && x1 <= left + width) {
    ctx.moveTo(x1, top); ctx.lineTo(x1, top + height);
  }
  ctx.strokeStyle = colors.passbandOutline;
  ctx.lineWidth = 2;
  ctx.stroke();
  ctx.strokeStyle = colors.passbandEdge;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.restore();
}
