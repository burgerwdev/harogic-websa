// Canvas layout
import { W, H, MARGIN } from '../core/store';
export { W, H, MARGIN };
export function plotRect() {
  return { x: MARGIN.left, y: MARGIN.top, w: W - MARGIN.left - MARGIN.right, h: H - MARGIN.top - MARGIN.bottom };
}
