// Absolute measurements must retain the selected trace's mode and acquisition family.
import * as S from '../core/store';
import { getTraceDisplay } from './peaks';

export function getActiveAbsolutePowers(): Float32Array | null {
  const trace = S.traces[S.activeTraceIdx];
  if (!trace || trace.mode === 'OFF') return null;
  if (S.rtaMode) return S.rtaDisplays[S.activeTraceIdx] ?? null;
  if (trace.isNormalized) return null;
  return getTraceDisplay(trace);
}
