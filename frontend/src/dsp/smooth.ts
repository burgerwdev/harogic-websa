import { smoothBins } from '../ui/displayState';
// Smoothing: 2nd-order SG + gradient-adaptive / window smoothing

let gradients = new Float64Array(0);

function gradientThreshold(src: Float32Array): number {
  if (gradients.length !== src.length - 2) gradients = new Float64Array(src.length - 2);
  let count = 0;
  for (let i = 1; i < src.length - 1; i++) {
    const value = Math.abs(src[i + 1] - src[i - 1]);
    if (Number.isFinite(value)) gradients[count++] = value;
  }
  if (!count) return Infinity;
  const rank = Math.floor(count * 0.9);
  let lo = 0, hi = count - 1, budget = count * 8;
  // Three-way selection keeps flat noise/plateaus linear without sorting a frame.
  while (lo < hi) {
    if ((budget -= hi - lo + 1) < 0) {
      gradients.subarray(lo, hi + 1).sort();
      break;
    }
    const a = gradients[lo], b = gradients[(lo + hi) >>> 1], c = gradients[hi];
    const pivot = Math.max(Math.min(a, b), Math.min(Math.max(a, b), c));
    let left = lo, i = lo, right = hi;
    while (i <= right) {
      const value = gradients[i];
      if (value < pivot) { gradients[i] = gradients[left]; gradients[left++] = value; i++; }
      else if (value > pivot) { gradients[i] = gradients[right]; gradients[right--] = value; }
      else i++;
    }
    if (rank < left) hi = left - 1;
    else if (rank > right) lo = right + 1;
    else break;
  }
  return gradients[rank];
}

function fitGeometry(first: number, last: number) {
  let sx = 0, sx2 = 0, sx3 = 0, sx4 = 0;
  for (let x = first; x <= last; x++) {
    sx += x; sx2 += x * x; sx3 += x * x * x; sx4 += x * x * x * x;
  }
  const a = sx2 * sx4 - sx3 * sx3, b = sx * sx4 - sx3 * sx2, c = sx * sx3 - sx2 * sx2;
  return { a, b, c, det: (last - first + 1) * a - sx * b + sx2 * c };
}

// Savitzky-Golay second-order smoothing preserves peaks and edges.
export function sgSmooth(src: Float32Array, w: number, adaptive: boolean): Float32Array {
  if (w <= 1) return src.slice();
  const out = new Float32Array(src.length);
  const th = adaptive && src.length > 20 ? gradientThreshold(src) : Infinity;
  const half = (w - 1) >> 1, smallHalf = (Math.min(3, w) - 1) >> 1;
  const fullFit = fitGeometry(-half, half), smallFit = fitGeometry(-smallHalf, smallHalf);
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
    const fit = s === i - h2 && e === i + h2 ? ww === w ? fullFit : smallFit : fitGeometry(s - i, e - i);
    let sy = 0, sxy = 0, sx2y = 0;
    for (let j = s; j <= e; j++) {
      const x = j - i, y = src[j];
      sy += y; sxy += x * y; sx2y += x * x * y;
    }
    if (Math.abs(fit.det) < 1e-12) { out[i] = src[i]; continue; }
    out[i] = (sy * fit.a - sxy * fit.b + sx2y * fit.c) / fit.det;
  }
  return out;
}

export function smoothForDisplay(src: Float32Array, mode: string): Float32Array {
  const w = smoothBins.get();
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
