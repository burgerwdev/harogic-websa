// Normalization: reference build / direct-pass calibration / display-layer transform
import * as S from '../core/store';
import { resetTraceAccum } from './traces';
import { updateInfoBar } from '../render/infobar';

export function normRefWindow(): number {
  if (S.normRefWinUser > 0) return S.normRefWinUser;
  if (!S.freqArray || S.freqArray.length < 2) return 5;
  const binHz = S.freqArray[1] - S.freqArray[0];
  const spanHz = S.freqArray[S.freqArray.length - 1] - S.freqArray[0];
  let w = Math.round(S.NORM_REF_RBW_FACTOR * S.currentRBW / binHz);
  const maxByHz = Math.max(3, Math.floor(Math.min(0.02 * spanHz, 5e6) / binHz));
  w = Math.max(3, Math.min(w, maxByHz, 9));
  return w || 5;
}

export function smoothRefWindow(ref: Float32Array, win: number): Float32Array {
  const n = ref.length;
  const half = (win - 1) >> 1;
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    const s = Math.max(0, i - half), e = Math.min(n - 1, i + half);
    let sum = 0;
    for (let j = s; j <= e; j++) sum += ref[j];
    out[i] = sum / (e - s + 1);
  }
  return out;
}

function classifySource(powers: Float32Array) {
  const n = powers.length;
  const sorted = Array.from(powers).sort((a, b) => a - b);
  const noiseFloor = sorted[Math.floor(n * 0.3)];
  const thresh = noiseFloor + 20;
  const isSrc = new Uint8Array(n);
  for (let i = 0; i < n; i++) if (powers[i] > thresh) isSrc[i] = 1;
  let srcCount = 0;
  for (let i = 0; i < n; i++) srcCount += isSrc[i];
  return { isSrc, noiseFloor, thresh, srcRatio: n ? srcCount / n : 0 };
}

function buildReferenceTable(powers: Float32Array): Float32Array {
  const n = powers.length;
  const { isSrc, srcRatio } = classifySource(powers);
  // No source points (noise source/weak signal): reference = smoothed noise floor (local mean),
  // not a single-frame raw snapshot — otherwise the normalized difference jumps with
  // per-frame noise and the baseline fails to converge at 0
  if (srcRatio < 0.05) {
    const win = Math.max(3, Math.min(9, normRefWindow()));
    return smoothRefWindow(powers, win);
  }
  const ref = new Float32Array(n);
  const K = 8;
  for (let i = 0; i < n; i++) {
    if (isSrc[i]) { ref[i] = powers[i]; continue; }
    const vals: number[] = [];
    let r = 0;
    while (vals.length < K && r < n) {
      r++;
      if (i - r >= 0 && isSrc[i - r]) vals.push(powers[i - r]);
      if (i + r < n && isSrc[i + r]) vals.push(powers[i + r]);
    }
    if (vals.length) {
      vals.sort((a, b) => a - b);
      ref[i] = vals[vals.length >> 1];
    } else {
      ref[i] = powers[i];
    }
  }
  return ref;
}

function removeOutliers(p: Float32Array, thresh = 15): Float32Array {
  let out = p;
  for (let i = 1; i < p.length - 1; i++) {
    const v = p[i], l = p[i - 1], r = p[i + 1];
    if (isFinite(v) && isFinite(l) && isFinite(r) &&
      (v < l - thresh && v < r - thresh || v > l + thresh && v > r + thresh)) {
      if (out === p) out = new Float32Array(p);
      out[i] = (l + r) / 2;
    }
  }
  return out;
}

function cleanReference(ref: Float32Array, win = 5): Float32Array {
  const n = ref.length;
  const out = new Float32Array(ref);
  for (let i = 0; i < n; i++) {
    const s = Math.max(0, i - win), e = Math.min(n - 1, i + win);
    const vals: number[] = [];
    for (let j = s; j <= e; j++) if (j !== i) vals.push(ref[j]);
    vals.sort((a, b) => a - b);
    const med = vals[vals.length >> 1];
    if (ref[i] < med - 10) out[i] = med;
  }
  return out;
}

export function normalizeActiveTrace() {
  const t = S.traces[S.activeTraceIdx];
  if (!t.powers) return;
  if (t.reference && t.isNormalized) { resetActiveTraceNormalize(); return; }
  t.reference = cleanReference(buildReferenceTable(t.powers));
  t.isNormalized = true;
  t._settling = true;
  t._lastAbsorb = performance.now();
  resetTraceAccum(t);
  S.setDisplayUnit('dB');
  S.setDisplayRef(0.0);
  updateNormalizeStatusUI();
  updateInfoBar();
}

export function resetActiveTraceNormalize() {
  const t = S.traces[S.activeTraceIdx];
  t.reference = null; t.isNormalized = false;
  resetTraceAccum(t);
  if (!S.traces.some(x => x.isNormalized && x.reference)) {
    S.setDisplayUnit('dBm');
    S.setDisplayRef(S.refLevel);
  }
  updateNormalizeStatusUI();
  updateInfoBar();
}

export function updateNormalizeStatusUI() {
  const t = S.traces[S.activeTraceIdx];
  const btn = document.getElementById('btn-normalize');
  if (btn) btn.classList.toggle('active', !!(t.reference && t.isNormalized));
}
