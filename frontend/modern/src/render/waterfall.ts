// 瀑布图渲染: ImageData 逐像素密度着色 + 滚动历史
import * as S from '../core/store';
import { canvasColors, getTheme } from '../core/theme';

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

// 从原始行(任意宽度)降采样到瀑布显示宽度
function downsampleRow(src: Uint16Array, w: number): Uint16Array {
  if (src.length === w) return src;
  const out = new Uint16Array(w);
  for (let i = 0; i < w; i++) {
    const j0 = Math.floor(i * src.length / w);
    const j1 = Math.min(src.length - 1, Math.ceil((i + 1) * src.length / w));
    let m = 0;
    for (let j = j0; j < j1; j++) if (src[j] > m) m = src[j];   // 保峰
    out[i] = m;
  }
  return out;
}

// 渲染瀑布到指定 canvas(覆盖 marker 表区域)
export function renderWaterfall(canvas: HTMLCanvasElement, maxDensity: number) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const W = canvas.width, H = canvas.height;
  const rows = S.waterfallRows;
  const n = rows.length;
  if (!n) { ctx.fillStyle = canvasColors().bg; ctx.fillRect(0, 0, W, H); return; }
  const img = ctx.createImageData(W, H);
  const px = img.data;
  const lut = densityLUT();
  const dmax = Math.max(1, maxDensity || 20);
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
    const ds = downsampleRow(row, W);
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

// RTA 模式: 从实时 spec 生成瀑布行, 按帧噪底动态映射 —— 底噪稳定显示为暗蓝(可见),
// 信号随强度渐变为红。避免固定 -110~-30 映射在窄 span(dec 大, RBW 窄 -> 噪底更低)时
// 把底噪压成全黑、只剩信号满红的两个极端。
export function pushRtaRow(spec: Float32Array, w: number, maxDensity: number) {
  const sorted = Array.from(spec).filter(isFinite).sort((a, b) => a - b);
  if (sorted.length < 4) return;
  const floor = sorted[Math.floor(sorted.length * 0.3)];
  const peak = sorted[Math.floor(sorted.length * 0.98)];
  const dyn = Math.max(15, peak - floor);
  const row = new Uint16Array(w);
  for (let i = 0; i < w; i++) {
    const j0 = Math.floor(i * spec.length / w);
    const j1 = Math.min(spec.length - 1, Math.ceil((i + 1) * spec.length / w));
    let m = -300;
    for (let j = j0; j < j1; j++) if (isFinite(spec[j]) && spec[j] > m) m = spec[j];
    // noise floor lands ~26% of the LUT (clearly visible blue), a full dyn rise saturates red
    const frac = (m - floor) / dyn;
    const lvl = Math.max(0, Math.min(maxDensity, Math.round((0.26 + frac * 0.74) * maxDensity)));
    row[i] = lvl;
  }
  S.pushWaterfallRow(row);
}

// SWP 模式: 从迹线生成瀑布行(保峰降采样)并累积(节流调用方控制)
export function pushSwpRow(powers: Float32Array, w: number, maxDensity: number) {
  const row = new Uint16Array(w);
  for (let i = 0; i < w; i++) {
    const j0 = Math.floor(i * powers.length / w);
    const j1 = Math.min(powers.length - 1, Math.ceil((i + 1) * powers.length / w));
    let m = -300;
    for (let j = j0; j < j1; j++) if (powers[j] > m) m = powers[j];
    // 映射 dBm → 密度(0~maxDensity): 固定 -110(底噪) ~ -30(强信号) 动态范围
    const lvl = Math.max(0, Math.min(maxDensity, Math.round((m + 110) / 80 * maxDensity)));
    row[i] = lvl;
  }
  S.pushWaterfallRow(row);
}
