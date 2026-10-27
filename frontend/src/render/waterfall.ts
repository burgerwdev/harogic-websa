import { drawPassband } from './passband';
// 瀑布图渲染: ImageData 逐像素密度着色 + 滚动历史
import * as S from '../core/store';
import { canvasColors, getTheme } from '../core/theme';
import { percentileApprox } from '../dsp/stats';
import { wfHiDbm, wfLoDbm, wfRangeMode } from '../ui/waterfallState';
import { getCapture, getView, isZoomed, viewportVersion } from '../ui/spectrumViewport';
import { lowerBound, projectSpectrum, sameFrequencyGrid } from '../dsp/grid';
import { sdrBadgeDragFrequency, sdrSpectrumWindow } from '../ui/sdrBadgeGesture';
import { demodBandHz, sdrDemod, sdrPitchHz } from '../ui/sdrState';
import { filterPreviewHz } from '../ui/sdrFilter';
import { passbandDragFrequency } from '../ui/sdrFilterGesture';
import { getDisplayRange, getDisplayTop, hasDisplayBounds } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';

const MAX_HISTORY_COLUMNS = 16384;
type RowGeometry = {
	freq: Float64Array | null; lo: number; hi: number;
	/** SDR stores raw amplitude codes, with zero reserved for missing observations. */
	level?: { lo: number; step: number; windowLo: number; windowHi: number; offset: boolean };
};
const rowGeometry = new WeakMap<Uint16Array, RowGeometry>();
let historyFreq: Float64Array | null = null;
let historyScratch = new Float32Array(0);
// One current display mapping per retained axis, never a cache of zoom histories.
const rowMappings = new WeakMap<Float64Array, { w: number; lo: number; hi: number; first: Int32Array; end: Int32Array }>();

/** Stored native rows use their true Hz; only over-budget rows use uniform columns. */
export function sliceWaterfallRow(row: Uint16Array, w: number, lo: number, hi: number,
	out?: Uint16Array): Uint16Array {
	const geometry = rowGeometry.get(row);
	if (!geometry) return sliceDensityRow(row, w, 0, 1, out);
	const { freq, lo: capLo, hi: capHi } = geometry;
	if (!freq) return sliceDensityRow(row, w, (lo - capLo) / (capHi - capLo), (hi - capLo) / (capHi - capLo), out);
	const dst = out && out.length === w ? out : new Uint16Array(w);
	let mapping = rowMappings.get(freq);
	if (!mapping || mapping.w !== w || mapping.lo !== lo || mapping.hi !== hi) {
		const first = new Int32Array(w), end = new Int32Array(w);
		for (let x = 0; x < w; x++) {
			const a = lo + x / w * (hi - lo), b = lo + (x + 1) / w * (hi - lo);
			first[x] = lowerBound(freq, a);
			end[x] = b >= freq[freq.length - 1] ? freq.length : lowerBound(freq, b);
			if (first[x] !== end[x]) continue;
			const mid = (a + b) / 2;
			const left = Math.max(0, first[x] - 1), right = Math.min(freq.length - 1, first[x]);
			const nearest = mid - freq[left] <= freq[right] - mid ? left : right;
			const cellLo = nearest ? (freq[nearest - 1] + freq[nearest]) / 2 : freq[0];
			const cellHi = nearest + 1 < freq.length ? (freq[nearest] + freq[nearest + 1]) / 2 : freq[nearest];
			if (mid >= cellLo && mid <= cellHi) { first[x] = nearest; end[x] = nearest + 1; }
		}
		mapping = { w, lo, hi, first, end };
		rowMappings.set(freq, mapping);
	}
	for (let x = 0; x < w; x++) {
		let peak = 0;
		for (let i = mapping.first[x]; i < mapping.end[x]; i++) if (row[i] > peak) peak = row[i];
		dst[x] = peak;
	}
	return dst;
}

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

// Reused scratch state: the waterfall redraws on every render tick while rows arrive much
// more slowly, so buffers are kept instead of allocating an ImageData per frame.
let wfImg: ImageData | null = null;
let wfImgKey = '';
let wfLastPushes = -1;
let wfLastMax = -1;
let wfLastTheme = '';
let sliceScratch: Uint16Array | null = null;
let wfNewestRow: Uint16Array | null = null;
let wfCanvas: HTMLCanvasElement | null = null;
let wfOverlayX: number | null = null;

/** Paint over the clean history image; never write the indicator into cached pixels. */
function paintBadgeIndicator(ctx: CanvasRenderingContext2D, width: number, height: number): void {
  const frequency = sdrBadgeDragFrequency() ?? passbandDragFrequency();
  const view = frequency === null ? null : sdrSpectrumWindow();
  wfOverlayX = frequency !== null && view && frequency >= view.lo && frequency <= view.hi
    ? Math.max(0, Math.min(width - 1, Math.round((frequency - view.lo) / (view.hi - view.lo) * width))) : null;
  if (wfOverlayX === null || frequency === null || !view) return;
  const [bandLo, bandHi] = demodBandHz(sdrDemod.get(), filterPreviewHz(), sdrPitchHz.get());
  const scale = width / (view.hi - view.lo);
  const left = (frequency + bandLo - view.lo) * scale;
  const right = (frequency + bandHi - view.lo) * scale;
  ctx.save();
  const colors = canvasColors();
  drawPassband(ctx, left, right, 0, 0, width, height, colors, true);
  ctx.globalAlpha = 1;
  ctx.fillStyle = colors.rta;
  ctx.fillRect(wfOverlayX, 0, Math.min(2, width - wfOverlayX), height);
  ctx.restore();
}

/**
 * Map one row's stored columns onto the canvas width through the visible capture
 * fraction [f0,f1]. Downscale (source range wider than a pixel) aggregates with MAX —
 * the same peak-hold semantics the rows were built with; upscale picks the nearest
 * column (a step, never an interpolation: the waterfall must not invent detail).
 * Fraction-based, so rows stored at different widths (a resize mid-session) mix safely.
 */
export function sliceDensityRow(row: Uint16Array, w: number, f0: number, f1: number,
	out?: Uint16Array): Uint16Array {
	const dst = out && out.length === w ? out : new Uint16Array(w);
	const len = row.length;
	const span = Math.max(0, f1 - f0);
	for (let x = 0; x < w; x++) {
		const a = f0 + (x / w) * span;
		const b = f0 + ((x + 1) / w) * span;
		let i0 = Math.floor(a * len);
		let i1 = Math.ceil(b * len);
		if (i1 <= i0) i1 = i0 + 1;
		i0 = Math.max(0, Math.min(len - 1, i0));
		i1 = Math.max(i0 + 1, Math.min(len, i1));
		let m = 0;
		for (let j = i0; j < i1; j++) if (row[j] > m) m = row[j];
		dst[x] = m;
	}
	return dst;
}

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
    wfCanvas = canvas;
    paintBadgeIndicator(ctx, W, H);
    return;
  }
  // Viewport slice: the waterfall follows the main plot's display window (design §2,
  // phase 2); fractions are stable across rows stored at different widths.
  const cap = getCapture();
  const zv = isZoomed() ? getView() : null;
  const view = S.sdrMode ? sdrSpectrumWindow() ?? cap : zv && cap
    && zv.lo >= cap.lo - 0.5 && zv.hi <= cap.hi + 0.5 ? zv : cap;
  let f0 = 0, f1 = 1;
  if (view && cap && cap.hi > cap.lo) {
    f0 = (view.lo - cap.lo) / (cap.hi - cap.lo);
    f1 = (view.hi - cap.lo) / (cap.hi - cap.lo);
  }
  const viewKey = `${view?.lo}:${view?.hi}:${f0.toFixed(4)}:${f1.toFixed(4)}:${viewportVersion()}`;
  const boundsKey = S.sdrMode ? `${hasDisplayBounds()}:${getDisplayTop()}:${getDisplayRange()}:${displayOffset.get()}` : '';
  const imageKey = `${W}x${H}:${wfRangeMode.get()}:${wfLoDbm.get()}:${wfHiDbm.get()}:${viewKey}:${boundsKey}`;
  const sameImage = wfCanvas === canvas && wfImg !== null && wfImgKey === imageKey && wfLastMax === dmax && wfLastTheme === theme;
  const pushed = S.waterfallPushes - wfLastPushes;
  if (sameImage && pushed === 0 && wfNewestRow === rows[n - 1]) {
    // Frozen history still has to erase the previous indicator before drawing the next.
    if (wfOverlayX !== null || sdrBadgeDragFrequency() !== null || passbandDragFrequency() !== null) {
      ctx.putImageData(wfImg!, 0, 0);
      paintBadgeIndicator(ctx, W, H);
    }
    return;
  }
  // Existing pixels already represent immutable historical rows. Scroll those pixels
  // once and rasterize only arrivals; zoom/resize/palette/reset rebuilds all rows.
  const scroll = sameImage && pushed > 0 && pushed < Math.min(n, H)
    && rows[n - 1 - pushed] === wfNewestRow ? pushed : 0;
  wfLastPushes = S.waterfallPushes;
  wfLastMax = dmax;
  wfLastTheme = theme;
  wfImgKey = imageKey;
  wfCanvas = canvas;
  wfNewestRow = rows[n - 1];
  if (!wfImg || wfImg.width !== W || wfImg.height !== H) wfImg = ctx.createImageData(W, H);
  const img = wfImg;
  const px = img.data;
  if (scroll) px.copyWithin(scroll * W * 4, 0, (H - scroll) * W * 4);
  const lut = densityLUT();
  const bg = canvasColors().bg;
  const bgPx = [parseInt(bg.slice(1, 3), 16), parseInt(bg.slice(3, 5), 16), parseInt(bg.slice(5, 7), 16)];
  for (let y = 0; y < (scroll || H); y++) {
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
    if (!sliceScratch || sliceScratch.length !== W) sliceScratch = new Uint16Array(W);
    const ds = view ? sliceWaterfallRow(row, W, view.lo, view.hi, sliceScratch)
      : sliceDensityRow(row, W, f0, f1, sliceScratch);
    const level = rowGeometry.get(row)?.level;
    const displayBounds = S.sdrMode && hasDisplayBounds();
    const fixedBounds = wfRangeMode.get() === 'fixed';
    const top = displayBounds ? getDisplayTop() - displayOffset.get() : fixedBounds ? wfHiDbm.get() : level?.windowHi ?? 0;
    const bottom = displayBounds ? top - getDisplayRange() : fixedBounds ? wfLoDbm.get() : level?.windowLo ?? 0;
    for (let x = 0; x < W; x++) {
      let fraction = ds[x] / dmax;
      if (level) {
        fraction = ds[x] === 0 ? 0 : (level.lo + (ds[x] - 1) * level.step - bottom) / (top - bottom);
        if (!displayBounds && !fixedBounds && ds[x] !== 0 && level.offset) fraction = 0.26 + fraction * 0.74;
      }
      const lvl = Math.max(0, Math.min(255, Math.round(fraction * 255)));
      const c = lut[lvl];
      const o = (y * W + x) * 4;
      px[o] = c & 0xff;
      px[o + 1] = (c >> 8) & 0xff;
      px[o + 2] = (c >> 16) & 0xff;
      px[o + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
  paintBadgeIndicator(ctx, W, H);
}

// Native rows retain one level per bin; oversized sources are peak-projected before this mapping.
// Both modes use the same dB window.
//   auto  : window = [frame floor (30th pct), frame max]; the floor is placed at 26% of the
//           palette so the noise texture stays visible even at very low noise floors
//   fixed : window = the user's dBm range, plain linear (colour = level)
function buildRow(levels: ArrayLike<number>, maxDensity: number, lo: number, hi: number, offset: boolean): Uint16Array {
  const span = hi - lo;
  const row = new Uint16Array(levels.length);
  for (let i = 0; i < levels.length; i++) {
    if (!Number.isFinite(levels[i])) continue;
    let frac = (levels[i] - lo) / span;
    if (offset) frac = 0.26 + frac * 0.74;
    row[i] = Math.max(0, Math.min(maxDensity, Math.round(Math.max(0, Math.min(1.26, frac)) * maxDensity)));
  }
  return row;
}

/** Resolve the dB window for the current range mode. */
function windowFor(levels: ArrayLike<number>): { lo: number; hi: number; offset: boolean } {
  if (wfRangeMode.get() === 'fixed') {
    const lo = wfLoDbm.get(), hi = wfHiDbm.get();
    if (Number.isFinite(lo) && Number.isFinite(hi) && lo < hi) {
      return { lo, hi, offset: false };
    }
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

/** Native bins survive local zoom, independent of canvas size; oversized SWP grids
 * are peak-projected into the existing 16384-column / 512-row history budget. */
export function pushSpectrumRow(levels: Float32Array, freq: Float64Array, lo: number, hi: number, maxDensity: number) {
  const win = windowFor(levels);
  let source = levels;
  let axis: Float64Array | null = null;
  if (levels.length <= MAX_HISTORY_COLUMNS) {
    if (!sameFrequencyGrid(historyFreq, freq)) historyFreq = freq;
    axis = historyFreq;
  } else {
    if (historyScratch.length !== MAX_HISTORY_COLUMNS) historyScratch = new Float32Array(MAX_HISTORY_COLUMNS);
    source = projectSpectrum(freq, levels, lo, hi, historyScratch);
  }
  let row: Uint16Array;
  const geometry: RowGeometry = { freq: axis, lo, hi };
  if (S.sdrMode) {
    let min = Infinity, max = -Infinity;
    for (let i = 0; i < source.length; i++) {
      if (!Number.isFinite(source[i])) continue;
      min = Math.min(min, source[i]); max = Math.max(max, source[i]);
    }
    if (!Number.isFinite(min)) min = max = 0;
    const step = (max - min) / 65534 || 1;
    row = new Uint16Array(source.length);
    for (let i = 0; i < source.length; i++) {
      if (Number.isFinite(source[i])) row[i] = 1 + Math.round((source[i] - min) / step);
    }
    geometry.level = { lo: min, step, windowLo: win.lo, windowHi: win.hi, offset: win.offset };
  } else {
    row = buildRow(source, maxDensity, win.lo, win.hi, win.offset);
  }
  rowGeometry.set(row, geometry);
  S.pushWaterfallRow(row);
}
