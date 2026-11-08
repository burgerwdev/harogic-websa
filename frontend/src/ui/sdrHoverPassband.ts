import * as S from '../core/store';
import { requestRender } from '../render/redraw';
import { sdrLocateHover } from './sdrLocate';

let frequency: number | null = null;
/** Shared by both plots; leaving either plot clears the preview on both. */
export function setSdrHoverFrequency(hz: number | null): void {
  const next = hz !== null && Number.isFinite(hz) && hz > 0 ? hz : null;
  if (next === frequency) return;
  frequency = next; requestRender();
}
export function sdrHoverFrequency(): number | null {
  return S.sdrMode && S.deviceConnected && frequency !== null ? sdrLocateHover()?.targetHz ?? frequency : null;
}
