// Canvas layout
import { W, H, MARGIN } from '../core/store';
import * as S from '../core/store';
import { getDisplayRange, getDisplayTop } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';
import { xAxisTransform, yAxisTransform } from '../ui/axisDrag';
import { getCapture, getView, isZoomed } from '../ui/spectrumViewport';
export { W, H, MARGIN };
export function plotRect() {
  const top = S.sdrMode ? Math.max(MARGIN.top, 28) : MARGIN.top;
  return { x: MARGIN.left, y: top, w: W - MARGIN.left - MARGIN.right, h: H - top - MARGIN.bottom };
}

export function getY(val: number): number {
  if (isFinite(val)) val += displayOffset.get();
  const top = getDisplayTop(), bottom = top - getDisplayRange();
  if (!isFinite(val)) val = bottom - 10;
  const rect = plotRect();
  const y = rect.y + ((top - val) / (top - bottom)) * rect.h;
  // A level-axis gesture displaces the trace through the same mapping everything else uses, so
  // markers, overlays and the trace cannot disagree while the pointer is down (ui/axisDrag.ts).
  const t = yAxisTransform();
  if (!t) return y;
  return rect.y + (y - rect.y) * t.sy + t.dyFrac * rect.h;
}

/**
 * True when a sample is a level that a trace can draw.
 *
 * A non-finite sample is not a level. `getY()` puts a non-finite sample below the graticule.
 * A caller that ignores this rule draws a vertical line down the plot edge. The SDR panadapter
 * publishes NaN at an edge of its capture grid.
 *
 * A level below -300 dBm is not physical either. It is the same "no data" marker: the swept path
 * writes NaN, but another driver can write a low sentinel. Both trace renderers lift the pen at
 * such a sample.
 */
export function plottableLevel(v: number): boolean {
  return isFinite(v) && v > -300;
}

export function getX(idx: number, points: number): number {
  const rect = plotRect();
  // Every overlay and hit test uses the native Hz, including partial capture coverage.
  const view = isZoomed() ? getView() : null;
  const cap = getCapture() ?? (S.rtaMode && S.rtaData
    ? { lo: S.rtaData.startHz, hi: S.rtaData.stopHz } : null);
  const win = view ?? cap;
  const fa = S.freqArray;
  let x: number;
  if (win && fa && fa.length === points && points > 1) {
    const index = Math.max(0, Math.min(points - 1, idx));
    const left = Math.floor(index), right = Math.min(points - 1, left + 1);
    // Measurement overlays can lie between bins; interpolate the coordinate only.
    const f = fa[left] + (fa[right] - fa[left]) * (index - left);
    x = rect.x + (f - win.lo) / (win.hi - win.lo) * rect.w;
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


