// Canvas layout
import { W, H, MARGIN } from '../core/store';
import * as S from '../core/store';
import { getDisplayRef } from '../ui/displayRef';
export { W, H, MARGIN };
export function plotRect() {
  return { x: MARGIN.left, y: MARGIN.top, w: W - MARGIN.left - MARGIN.right, h: H - MARGIN.top - MARGIN.bottom };
}

export function getY(val: number): number {
  if (isFinite(val)) val += S.displayOffset;
  const dispRef = getDisplayRef();
  const top = dispRef, bottom = dispRef - S.totalDivs * S.dbPerDiv;
  if (!isFinite(val)) val = bottom - 10;
  const rect = plotRect();
  return rect.y + ((top - val) / (top - bottom)) * rect.h;
}
export function getX(idx: number, points: number): number {
  const rect = plotRect();
  return rect.x + (idx / (points - 1)) * rect.w;
}


