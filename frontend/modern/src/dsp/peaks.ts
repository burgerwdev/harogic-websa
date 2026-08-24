// DSP 寻峰寻谷引擎: 三级寻峰 + 谷凹陷合并 + 频率方向遍历
import * as S from '../core/store';
import type { ExtremaItem } from '../core/store';
import { smoothForDisplay } from './smooth';

// P1: 抛物线亚频点拟合
export function parabolaFit(p: ArrayLike<number>, k: number): { dk: number; y: number } {
  const y0 = p[k - 1], y1 = p[k], y2 = p[k + 1];
  if (!isFinite(y0) || !isFinite(y1) || !isFinite(y2)) return { dk: 0, y: y1 };
  const denom = y0 - 2 * y1 + y2;
  if (Math.abs(denom) < 1e-9) return { dk: 0, y: y1 };
  const dk = -0.5 * (y2 - y0) / denom;
  const y = y1 - (y2 - y0) * (y2 - y0) / (8 * denom);
  return { dk, y };
}

// P3: Excursion —— 双侧追溯 ≥dL dB
export function hasExcursion(p: ArrayLike<number>, k: number, dL: number, isPeak: boolean): boolean {
  const v = p[k];
  let l = false, r = false;
  for (let i = k - 1; i >= 0; i--) {
    if (isPeak ? (v - p[i] >= dL) : (p[i] - v >= dL)) { l = true; break; }
    if (k - i > 100) break;
  }
  for (let i = k + 1; i < p.length; i++) {
    if (isPeak ? (v - p[i] >= dL) : (p[i] - v >= dL)) { r = true; break; }
    if (i - k > 100) break;
  }
  return l && r;
}

export function getTraceDisplay(t: S.TraceState): Float32Array | null {
  if (S.smoothBins > 1 && t.powers) return smoothForDisplay(t.powers, t.mode);
  return t.powers;
}

export function getDisplayPowers(): Float32Array | null {
  const t = S.traces[S.activeTraceIdx];
  if (t.powers) return getTraceDisplay(t);
  const any = S.traces.find(x => x.powers);
  return any ? getTraceDisplay(any) : null;
}

export function findExtremesOrdered(dir: string, isPeak: boolean): ExtremaItem[] {
  const t = S.traces[S.activeTraceIdx];
  const disp = getDisplayPowers();
  if (!disp) return [];
  const smoothed = S.smoothBins > 1;
  const raw = (t && t.powers) ? t.powers : disp;
  const thrEl = document.getElementById('input-peakthr') as HTMLInputElement;
  const thr = thrEl ? (parseFloat(thrEl.value) || -200) : -200;
  const isNorm = !!(t && t.reference && t.isNormalized);
  const EXCURSION = 6;
  const DEPTH = 3;
  const binHz = (S.freqArray && S.freqArray.length > 1) ? S.freqArray[1] - S.freqArray[0] : 0;
  const fa = S.freqArray || new Float64Array(disp.length);
  const res: ExtremaItem[] = [];
  for (let i = 1; i < disp.length - 1; i++) {
    const v = disp[i], l = disp[i - 1], r = disp[i + 1];
    if (!isFinite(v) || !isFinite(l) || !isFinite(r)) continue;
    if (isPeak) {
      if (v > thr && v >= l && v >= r && hasExcursion(disp, i, EXCURSION, true)) {
        const fit = parabolaFit(disp, i);
        res.push({ i, v, f: fa[i] + fit.dk * binHz, a: fit.y });
      }
    } else {
      const isV = isNorm ? (v < -1.0 && v <= l && v <= r)
        : (v <= l && v <= r && Math.max(l - v, r - v) >= DEPTH);
      if (isV && hasExcursion(disp, i, EXCURSION, false)) {
        if (!smoothed) {
          const half = (S.smoothBins - 1) >> 1;
          const N = Math.max(2, half);
          let bi = i, bv = v;
          for (let j = Math.max(0, i - N); j <= Math.min(disp.length - 1, i + N); j++) {
            if (isFinite(raw[j]) && raw[j] < bv) { bv = raw[j]; bi = j; }
          }
          const fit = parabolaFit(raw, bi);
          res.push({ i: bi, v: bv, sv: v, f: fa[bi] + fit.dk * binHz, a: fit.y });
        } else {
          const fit = parabolaFit(disp, i);
          res.push({ i, v, sv: v, f: fa[i] + fit.dk * binHz, a: fit.y });
        }
      }
    }
  }
  res.sort((a, b) => a.i - b.i);
  const out: ExtremaItem[] = [];
  const dedupWin = isPeak ? 3 : 25;
  for (const pk of res) {
    let merged = false;
    for (let j = 0; j < out.length; j++) {
      if (Math.abs(pk.i - out[j].i) < dedupWin) {
        if (isPeak) {
          if (pk.v > out[j].v) out[j] = pk;
        } else {
          const lo = Math.max(0, Math.min(pk.i, out[j].i) - dedupWin);
          const hi = Math.min(disp.length - 1, Math.max(pk.i, out[j].i) + dedupWin);
          let bi = out[j].i, bv = disp[out[j].i];
          for (let k = lo; k <= hi; k++) {
            if (isFinite(disp[k]) && disp[k] < bv) { bv = disp[k]; bi = k; }
          }
          const fit = parabolaFit(disp, bi);
          out[j] = { i: bi, v: bv, sv: bv, f: fa[bi] + fit.dk * binHz, a: fit.y };
        }
        merged = true;
        break;
      }
    }
    if (!merged) out.push(pk);
  }
  return out;
}

export function nextExtreme(dir: string, isPeak: boolean) {
  const list = findExtremesOrdered(dir, isPeak);
  if (!list.length) return;
  const m = S.markers.find(x => x.id === S.activeMkrId);
  if (!m) return;
  let pos = -1, bd = 1e9;
  for (let j = 0; j < list.length; j++) {
    const d = Math.abs(list[j].i - m.idx);
    if (d <= 3 && d < bd) { bd = d; pos = j; }
  }
  if (pos < 0) {
    if (dir === 'right') {
      const nxt = list.find(x => x.i > m.idx && Math.abs(x.i - m.idx) >= 25)
        || list.find(x => x.i > m.idx) || list[0];
      setMarkerIdx(nxt.i, nxt.f);
    } else {
      const lefts = list.filter(x => x.i < m.idx);
      const far = lefts.filter(x => Math.abs(x.i - m.idx) >= 25);
      const nxt = far.length ? far[far.length - 1]
        : (lefts.length ? lefts[lefts.length - 1] : list[list.length - 1]);
      setMarkerIdx(nxt.i, nxt.f);
    }
    return;
  }
  const nxt = dir === 'right' ? list[(pos + 1) % list.length]
    : list[(pos - 1 + list.length) % list.length];
  setMarkerIdx(nxt.i, nxt.f);
}

export function setMarkerIdx(idx: number, freqHz?: number) {
  const m = S.markers.find(x => x.id === S.activeMkrId);
  if (!m) return;
  m.enabled = true;
  if (m.mode === 'OFF') m.mode = 'NORMAL';
  const maxI = S.freqArray ? S.freqArray.length - 1 : 1000;
  m.idx = Math.max(0, Math.min(maxI, Math.round(idx)));
  m.freq = (freqHz != null && isFinite(freqHz) && freqHz > 0) ? freqHz : markerFreqHz(m.idx);
  renderAll();
}

import { markerFreqHz } from '../core/markerCommon';
import { renderAll } from '../render/spectrum';
