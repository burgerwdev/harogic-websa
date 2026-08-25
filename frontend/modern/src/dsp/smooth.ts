// Smoothing: 2nd-order SG + gradient-adaptive / window smoothing
import * as S from '../core/store';

// P2: Savitzky-Golay 2nd-order smoothing (preserve peaks/edges)
export function sgSmooth(src: Float32Array, w: number, adaptive: boolean): Float32Array {
  if (w <= 1) return src.slice();
  const out = new Float32Array(src.length);
  let th = Infinity;
  if (adaptive && src.length > 20) {
    const gs: number[] = [];
    for (let i = 1; i < src.length - 1; i++) gs.push(Math.abs(src[i + 1] - src[i - 1]));
    gs.sort((a, b) => a - b);
    th = gs[Math.floor(gs.length * 0.9)];
  }
  for (let i = 0; i < src.length; i++) {
    let ww = w;
    if (adaptive && i > 0 && i < src.length - 1) {
      const g = Math.abs(src[i + 1] - src[i - 1]);
      if (g > th) ww = Math.min(3, w);
    }
    const h2 = (ww - 1) >> 1;
    const s = Math.max(0, i - h2), e = Math.min(src.length - 1, i + h2);
    const m = e - s + 1;
    if (m < 3) { out[i] = src[i]; continue; }
    let sx = 0, sx2 = 0, sx3 = 0, sx4 = 0, sy = 0, sxy = 0, sx2y = 0;
    for (let j = s; j <= e; j++) {
      const x = j - i, y = src[j];
      sx += x; sx2 += x * x; sx3 += x * x * x; sx4 += x * x * x * x;
      sy += y; sxy += x * y; sx2y += x * x * y;
    }
    const det = m * (sx2 * sx4 - sx3 * sx3) - sx * (sx * sx4 - sx3 * sx2) + sx2 * (sx * sx3 - sx2 * sx2);
    if (Math.abs(det) < 1e-12) { out[i] = src[i]; continue; }
    const detA = sy * (sx2 * sx4 - sx3 * sx3) - sxy * (sx * sx4 - sx3 * sx2) + sx2y * (sx * sx3 - sx2 * sx2);
    out[i] = detA / det;
  }
  return out;
}

export function smoothForDisplay(src: Float32Array, mode: string): Float32Array {
  const w = S.smoothBins;
  const half = (w - 1) >> 1;
  const out = new Float32Array(src.length);
  if (mode === 'MAX_HOLD' || mode === 'MIN_HOLD') {
    for (let i = 0; i < src.length; i++) {
      const s = Math.max(0, i - half), e = Math.min(src.length - 1, i + half);
      let v = src[s];
      if (mode === 'MAX_HOLD') {
        for (let j = s + 1; j <= e; j++) if (src[j] > v) v = src[j];
      } else {
        for (let j = s + 1; j <= e; j++) if (src[j] < v) v = src[j];
      }
      out[i] = v;
    }
  } else if (mode === 'MEDIAN') {
    const tmp = new Float32Array(w);
    for (let i = 0; i < src.length; i++) {
      const s = Math.max(0, i - half), e = Math.min(src.length - 1, i + half);
      let n = 0;
      for (let j = s; j <= e; j++) tmp[n++] = src[j];
      for (let a = 0; a < n - 1; a++) for (let b = a + 1; b < n; b++)
        if (tmp[b] < tmp[a]) { const t = tmp[a]; tmp[a] = tmp[b]; tmp[b] = t; }
      out[i] = tmp[n >> 1];
    }
  } else {
    return sgSmooth(src, w, true);
  }
  return out;
}
