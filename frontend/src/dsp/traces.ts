// Trace processing: explicit gap fill, normalization and accumulation.
import * as S from '../core/store';
import { refLevel } from '../ui/refState';
import { rawToDisplayRef, setDisplayRef } from '../ui/displayRef';
import { accumulateTrace, applyMode } from './accumulator';
import { updateNormalizeStatusUI } from './normalizeStatus';
import { updateTrackingMarkers } from './markerTracking';
import { displayUnit } from '../ui/displayState';

export function gapFill(raw: Float32Array): Float32Array {
  if (!S.currentGapFill) return raw;
  let hasGap = false;
  for (let i = 0; i < raw.length; i++) {
    if (!isFinite(raw[i])) { hasGap = true; break; }
  }
  if (!hasGap) return raw;
  const out = new Float32Array(raw);
  let i = 0;
  while (i < out.length) {
    if (isFinite(out[i])) { i++; continue; }
    let j = i;
    while (j < out.length && !isFinite(out[j])) j++;
    const lv = i > 0 ? out[i - 1] : -120;
    const rv = j < out.length ? out[j] : -120;
    for (let k = i; k < j; k++) {
      const t = j > i ? (k - i) / (j - i) : 0;
      out[k] = lv + (rv - lv) * t;
    }
    i = j + 1;
  }
  return out;
}


export function resetTraceAccum(t: S.TraceState) {
  t.raw = null; t.powers = null; t.avgSum = null; t.avgCount = 0;
}

export function invalidateAllTraces() {
  S.traces.forEach(t => {
    resetTraceAccum(t);
    t.reference = null; t.isNormalized = false;
  });
  displayUnit.set('dBm');
  // Trace invalidation cannot replace a manually owned viewport or the SDR display scale.
  setDisplayRef('mode', rawToDisplayRef(refLevel.get()));
  updateNormalizeStatusUI();
}

/**
 * Apply a trace mode without discarding data:
 * - OFF hides the trace but keeps its samples, so re-enabling resumes it.
 * - MAX/MIN/CLEAR_WRITE continue from the samples already displayed.
 * - AVERAGE seeds the accumulator with the current trace (count = 1).
 * Use resetTraceAccum() for the explicit Clear action.
 */
export function applyTraceMode(t: S.TraceState, mode: string) {
  applyMode(t, mode);
}

export function processTraces(rawPowers: Float32Array) {
  const data0 = gapFill(rawPowers);
  S.traces.forEach(t => {
    if (t.mode === 'OFF') return;

    if (t.raw && t.raw.length !== data0.length) {
      t.reference = null; t.isNormalized = false;
      t.avgSum = null; t.avgCount = 0;
      t.raw = new Float32Array(data0);
    } else if (!t.raw) {
      t.avgSum = null; t.avgCount = 0;
      t.raw = new Float32Array(data0);
    } else {
      t.raw.set(data0);
    }

    let data = t.raw;
    if (t.reference && t.isNormalized && t.reference.length === t.raw.length) {
      let noiseFloor = t._noiseFloorT || 0;
      if (t._settling) {
        // Update the settling noise-floor estimate at most twice per second.
        const now = performance.now();
        if (t._noiseFloorT == null || now - (t._floorAt || 0) >= 500) {
          const tmp = Float32Array.from(t.raw).sort();
          noiseFloor = tmp[Math.floor(t.raw.length * 0.15)];
          t._noiseFloorT = noiseFloor;
          t._floorAt = now;
        }
      }
      data = new Float32Array(t.raw.length);
      for (let i = 0; i < t.raw.length; i++) {
        const diff = t.raw[i] - t.reference[i];
        if (t._settling && diff > S.ABSORB_THRESH && t.raw[i] > noiseFloor + 10) {
          t.reference[i] = t.raw[i];
          data[i] = 0;
          t._lastAbsorb = performance.now();
        } else {
          data[i] = diff;
        }
      }
      for (let i = 0; i < data.length; i++)
        if (data[i] > S.NORM_POS_CAP) data[i] = S.NORM_POS_CAP;
    }

    accumulateTrace(t, data);
  });
  updateTrackingMarkers();
  updateNormalizeStatusUI();
}
