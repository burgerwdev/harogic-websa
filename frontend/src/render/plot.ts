// Canvas layout
import { W, H, MARGIN } from '../core/store';
import * as S from '../core/store';
import { getDisplayRef } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';
import { xAxisTransform, yAxisTransform } from '../ui/axisDrag';
import { getCapture, getView, isZoomed } from '../ui/spectrumViewport';
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
  // Display-only zoom: the WHOLE swept path goes through one mapping — data position ->
  // view window — so the trace, markers, peak marks, limit/channel/harmonic overlays and
  // the frequency row can never disagree (design §4.4). The identity path below is
  // bit-for-bit what it always was when no view is active.
  const view = isZoomed() ? getView() : null;
  const cap = getCapture();
  let x: number;
  if (view && cap && points > 1) {
    const fa = S.freqArray;
    const f = fa && fa.length > 1
      ? fa[Math.max(0, Math.min(points - 1, idx))]
      : cap.lo + (idx / (points - 1)) * (cap.hi - cap.lo);
    x = rect.x + (f - view.lo) / (view.hi - view.lo) * rect.w;
  } else {
    x = rect.x + (idx / (points - 1)) * rect.w;
  }
  const t = xAxisTransform();
  if (!t) return x;
  return rect.x + (x - rect.x) * t.sx + t.dxFrac * rect.w;
}

/**
 * The drawn point that is nearest to canvas x. This is the hit test of the cursor readout.
 *
 * The function searches getX() and does not compute the window again. A display zoom, a gesture
 * preview and a sweep all reach x through getX(). Thus the readout, the markers and the trace
 * agree on the bin under the pointer (design §4.4, one mapping).
 */
export function nearestIdxAtX(x: number, points: number): number {
  if (points < 2) return 0;
  if (x <= getX(0, points)) return 0;
  if (x >= getX(points - 1, points)) return points - 1;
  let lo = 0;
  let hi = points - 1;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (getX(mid, points) <= x) lo = mid; else hi = mid;
  }
  return (x - getX(lo, points)) <= (getX(hi, points) - x) ? lo : hi;
}


