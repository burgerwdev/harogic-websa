// Trace processing: resampling/gapFill/spur suppression/state machine
import * as S from '../core/store';
import { updateNormalizeStatusUI } from './normalize';

// Peak-preserving resampling
export function resampleTrace(src: Float32Array, newLen: number, isMaxHold: boolean): Float32Array {
  const out = new Float32Array(newLen);
  const oldLen = src.length;
  if (oldLen < 2 || newLen < 2) return new Float32Array(src.slice(0, newLen));
  for (let i = 0; i < newLen; i++) {
    const start = i * (oldLen - 1) / (newLen - 1);
    const end = (i + 1) * (oldLen - 1) / (newLen - 1);
    const j0 = Math.floor(start);
    const j1 = Math.min(oldLen - 1, Math.max(j0, Math.ceil(end)));
    let v = src[j0];
    for (let j = j0 + 1; j <= j1; j++) {
      v = isMaxHold ? Math.max(v, src[j]) : Math.min(v, src[j]);
    }
    out[i] = v;
  }
  return out;
}

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

export function fillSpurDips(p: Float32Array): Float32Array {
  const out = new Float32Array(p);
  for (let i = 1; i < p.length - 1; i++) {
    const v = out[i], l = out[i - 1], r = out[i + 1];
    if (isFinite(v) && isFinite(l) && isFinite(r) && v < l - 25 && v < r - 25) {
      out[i] = (l + r) / 2;
    }
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
  S.setDisplayUnit('dBm');
  S.setDbPerDiv(10.0);
  S.setDisplayRef(S.refLevel);
  updateNormalizeStatusUI();
}

export function processTraces(rawPowers: Float32Array) {
  let data0 = gapFill(rawPowers);
  data0 = fillSpurDips(data0);
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
      const srt = t._noiseFloorT || 0;
      let noiseFloor = srt;
      if (t._settling) {
        const tmp = Float32Array.from(t.raw).sort();
        noiseFloor = tmp[Math.floor(t.raw.length * 0.15)];
        t._noiseFloorT = noiseFloor;
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

    if (t.mode === 'VIEW') {
      if (!t.powers || t.powers.length !== data.length) t.powers = new Float32Array(data);
      return;
    }

    if (!t.powers || t.powers.length !== data.length) {
      if (t.powers && (t.mode === 'MAX_HOLD' || t.mode === 'MIN_HOLD')
        && t.powers.length > 1 && data.length > 1) {
        t.powers = resampleTrace(t.powers, data.length, t.mode === 'MAX_HOLD');
      } else {
        t.avgSum = null; t.avgCount = 0;
        t.powers = new Float32Array(data);
        if (t.mode === 'AVERAGE') { t.avgSum = new Float32Array(data); t.avgCount = 1; }
      }
      return;
    }

    if (t.mode === 'CLEAR_WRITE') {
      t.powers.set(data);
    } else if (t.mode === 'MAX_HOLD') {
      const cap = (t.reference && t.isNormalized) ? S.NORM_POS_CAP : Infinity;
      for (let i = 0; i < data.length; i++) {
        const v = data[i];
        t.powers[i] = Math.max(t.powers[i], v < cap ? v : cap);
      }
    } else if (t.mode === 'MIN_HOLD') {
      for (let i = 0; i < data.length; i++) t.powers[i] = Math.min(t.powers[i], data[i]);
    } else if (t.mode === 'AVERAGE') {
      if (!t.avgSum || t.avgSum.length !== data.length) {
        t.avgSum = new Float32Array(data); t.avgCount = 1;
      } else {
        t.avgCount++;
        for (let i = 0; i < data.length; i++) {
          t.avgSum[i] += data[i];
          t.powers[i] = t.avgSum[i] / t.avgCount;
        }
      }
    }
  });
  updateNormalizeStatusUI();
}
