// 瀑布图渲染: ImageData 逐像素密度着色 + 滚动历史
import * as S from '../core/store';
import { canvasColors, getTheme } from '../core/theme';
import { percentileApprox } from '../dsp/stats';

// 密度 → 颜色 LUT(256 级), 按主题区分: dark=荧光系(深底亮色), light=深色系(浅底高对比)
let lutCache: { theme: string; lut: Uint32Array } | null = null;
function densityLUT(): Uint32Array {
  const th = getTheme();
  if (lutCache && lutCache.theme === th) return lutCache.lut;
  const stops = th === 'dark'
    ? [[0, 0, 0], [0, 0, 48], [0, 42, 128], [0, 96, 160], [0, 160, 192],
       [0, 192, 96], [64, 224, 64], [224, 224, 0], [255, 112, 0], [208, 0, 0]]
    : [[255, 255, 255], [224, 236, 255], [160, 200, 255], [96, 150, 255],
       [40, 100, 220], [60, 120, 160], [32, 140, 80], [200, 160, 0], [220, 80, 0], [180, 0, 0]];
  const lut = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    const x = i / 255 * (stops.length - 1);
    const j = Math.min(stops.length - 2, Math.floor(x));
    const t = x - j;
    const a = stops[j], b = stops[j + 1];
    const r = Math.round(a[0] + (b[0] - a[0]) * t);
    const g = Math.round(a[1] + (b[1] - a[1]) * t);
    const bl = Math.round(a[2] + (b[2] - a[2]) * t);
    lut[i] = (255 << 24) | (bl << 16) | (g << 8) | r;   // ABGR (little-endian)
  }
  lutCache = { theme: th, lut };
  return lut;
}

// 从原始行(任意宽度)降采样到瀑布显示宽度(复用 scratch 缓冲区，避免每行分配)
function downsampleRow(src: Uint16Array, w: number, reuse = false): Uint16Array {
  if (src.length === w) return src;
  let out: Uint16Array;
  if (reuse) {
    if (!wfRowScratch || wfRowScratch.length !== w) wfRowScratch = new Uint16Array(w);
    out = wfRowScratch;
  } else {
    out = new Uint16Array(w);
  }
  for (let i = 0; i < w; i++) {
    const j0 = Math.floor(i * src.length / w);
    const j1 = Math.min(src.length - 1, Math.ceil((i + 1) * src.length / w));
    let m = 0;
    for (let j = j0; j < j1; j++) if (src[j] > m) m = src[j];   // 保峰
    out[i] = m;
  }
  return out;
}

// Reused scratch state: the waterfall redraws on every render tick while rows arrive much
// more slowly, so buffers are kept instead of allocating an ImageData per frame.
let wfImg: ImageData | null = null;
let wfImgKey = '';
let wfLastPushes = -1;
let wfLastMax = -1;
let wfLastTheme = '';
let wfRowScratch: Uint16Array | null = null;

// 渲染瀑布到指定 canvas(覆盖 marker 表区域)
export function renderWaterfall(canvas: HTMLCanvasElement, maxDensity: number) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const W = canvas.width, H = canvas.height;
  const rows = S.waterfallRows;
  const n = rows.length;
  const theme = getTheme();
  const dmax = Math.max(1, maxDensity || 20);
  if (!n) {
    ctx.fillStyle = canvasColors().bg; ctx.fillRect(0, 0, W, H);
    wfLastPushes = S.waterfallPushes;
    return;
  }
  // Nothing new since the previous frame (renders are faster than row pushes) -> keep
  // the existing canvas content instead of rebuilding ~100k pixels.
  if (
    wfLastPushes === S.waterfallPushes && wfLastMax === dmax && wfLastTheme === theme
    && wfImgKey === `${W}x${H}:${S.wfRangeMode}:${S.wfLoDbm}:${S.wfHiDbm}`
  ) {
    return;
  }
  wfLastPushes = S.waterfallPushes;
  wfLastMax = dmax;
  wfLastTheme = theme;
  wfImgKey = `${W}x${H}:${S.wfRangeMode}:${S.wfLoDbm}:${S.wfHiDbm}`;
  if (!wfImg || wfImg.width !== W || wfImg.height !== H) wfImg = ctx.createImageData(W, H);
  const img = wfImg;
  const px = img.data;
  const lut = densityLUT();
  const bg = canvasColors().bg;
  const bgPx = [parseInt(bg.slice(1, 3), 16), parseInt(bg.slice(3, 5), 16), parseInt(bg.slice(5, 7), 16)];
  for (let y = 0; y < H; y++) {
    // Newest row at TOP, older rows scroll down (top-down growth)
    let row: Uint16Array | null = null;
    if (n <= H) {
      if (y >= n) { row = null; } else { row = rows[n - 1 - y]; }
    } else {
      row = rows[n - 1 - y];
    }
    if (!row) {
      for (let x = 0; x < W; x++) {
        const o = (y * W + x) * 4;
        px[o] = bgPx[0]; px[o + 1] = bgPx[1]; px[o + 2] = bgPx[2]; px[o + 3] = 255;
      }
      continue;
    }
    const ds = downsampleRow(row, W, true);
    for (let x = 0; x < W; x++) {
      const lvl = Math.min(255, Math.round(ds[x] / dmax * 255));
      const c = lut[lvl];
      const o = (y * W + x) * 4;
      px[o] = c & 0xff;
      px[o + 1] = (c >> 8) & 0xff;
      px[o + 2] = (c >> 16) & 0xff;
      px[o + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
}

// One mapping for both modes (see ui/wfRange.ts): the row is a peak-held downsample of the
// level array through a dB window.
//   auto  : window = [frame floor (30th pct), frame max]; the floor is placed at 26% of the
//           palette so the noise texture stays visible even at very low noise floors
//   fixed : window = the user's dBm range, plain linear (colour = level)
function buildRow(levels: ArrayLike<number>, w: number, maxDensity: number, lo: number, hi: number, offset: boolean): Uint16Array {
  const span = Math.max(1e-6, hi - lo);
  const row = new Uint16Array(w);
  for (let i = 0; i < w; i++) {
    const j0 = Math.floor(i * levels.length / w);
    const j1 = Math.min(levels.length - 1, Math.ceil((i + 1) * levels.length / w));
    let m = -Infinity;
    for (let j = j0; j < j1; j++) {
      const v = levels[j];
      if (Number.isFinite(v) && v > m) m = v;
    }
    let frac = Number.isFinite(m) ? (m - lo) / span : 0;
    if (offset) frac = 0.26 + frac * 0.74;
    const lvl = Math.max(0, Math.min(maxDensity, Math.round(Math.max(0, Math.min(1.26, frac)) * maxDensity)));
    row[i] = lvl;
  }
  return row;
}

/** Resolve the dB window for the current range mode. */
function windowFor(levels: ArrayLike<number>): { lo: number; hi: number; offset: boolean } {
  if (S.wfRangeMode === 'fixed') {
    const lo = Math.min(S.wfLoDbm, S.wfHiDbm - 1);
    return { lo, hi: S.wfHiDbm, offset: false };
  }
  const floor = percentileApprox(levels, 0.3);
  let peak = -Infinity;
  for (let i = 0; i < levels.length; i++) {
    const v = levels[i];
    if (v > peak) peak = v;                     // NaN never compares greater
  }
  if (!Number.isFinite(peak)) peak = floor + 15;
  const hi = Math.max(peak, floor + 15);        // never collapse below a 15 dB window
  return { lo: floor, hi, offset: true };
}

export function pushRtaRow(spec: Float32Array, w: number, maxDensity: number) {
  const win = windowFor(spec);
  S.pushWaterfallRow(buildRow(spec, w, maxDensity, win.lo, win.hi, win.offset));
}

export function pushSwpRow(powers: Float32Array, w: number, maxDensity: number) {
  const win = windowFor(powers);
  S.pushWaterfallRow(buildRow(powers, w, maxDensity, win.lo, win.hi, win.offset));
}

// The row width is owned by the renderer (the canvas is DPR/CSS dependent): building rows at
// this width removes the extra peak-hold rescale the RTA path used to need.
let rowWidth = 800;
export function setWaterfallRowWidth(w: number): void {
  if (Number.isFinite(w) && w > 0) rowWidth = Math.round(w);
}
export function waterfallRowWidth(): number {
  return rowWidth;
}
