// Canvas layout
import { W, H, MARGIN } from '../core/store';
import * as S from '../core/store';
import { getDisplayRef } from '../ui/displayRef';
export { W, H, MARGIN };
export function plotRect() {
  return { x: MARGIN.left, y: MARGIN.top, w: W - MARGIN.left - MARGIN.right, h: H - MARGIN.top - MARGIN.bottom };
}

export const PLOT_RECT = {
  x: MARGIN.left,
  y: MARGIN.top,
  w: W - MARGIN.left - MARGIN.right,
  h: H - MARGIN.top - MARGIN.bottom,
};

export function getY(val: number): number {
  if (isFinite(val)) val += S.displayOffset;
  const dispRef = getDisplayRef();
  const top = dispRef, bottom = dispRef - S.totalDivs * S.dbPerDiv;
  if (!isFinite(val)) val = bottom - 10;
  return PLOT_RECT.y + ((top - val) / (top - bottom)) * PLOT_RECT.h;
}
export function getX(idx: number, points: number): number {
  return PLOT_RECT.x + (idx / (points - 1)) * PLOT_RECT.w;
}


