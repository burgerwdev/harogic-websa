// Canvas layout
import { W, H, MARGIN } from '../core/store';
import * as S from '../core/store';
import { getDisplayRef } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';
import { xAxisTransform, yAxisTransform } from '../ui/axisDrag';
export { W, H, MARGIN };
export function plotRect() {
  return { x: MARGIN.left, y: MARGIN.top, w: W - MARGIN.left - MARGIN.right, h: H - MARGIN.top - MARGIN.bottom };
}

export function getY(val: number): number {
  if (isFinite(val)) val += displayOffset.get();
  const dispRef = getDisplayRef();
  const top = dispRef, bottom = dispRef - S.totalDivs * S.dbPerDiv;
  if (!isFinite(val)) val = bottom - 10;
  const rect = plotRect();
  const y = rect.y + ((top - val) / (top - bottom)) * rect.h;
  // A level-axis gesture displaces the trace through the same mapping everything else uses, so
  // markers, overlays and the trace cannot disagree while the pointer is down (ui/axisDrag.ts).
  const t = yAxisTransform();
  if (!t) return y;
  return rect.y + (y - rect.y) * t.sy + t.dyFrac * rect.h;
}
export function getX(idx: number, points: number): number {
  const rect = plotRect();
  const x = rect.x + (idx / (points - 1)) * rect.w;
  const t = xAxisTransform();
  if (!t) return x;
  return rect.x + (x - rect.x) * t.sx + t.dxFrac * rect.w;
}


