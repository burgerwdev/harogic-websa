import { drawPassband } from './passband';
import { renderSdrChannelBands } from './sdrChannelBands';
import { getMarqueePreview } from '../ui/spectrumZoomGestures';
// 瀑布图渲染: ImageData 逐像素密度着色 + 滚动历史
import * as S from '../core/store';
import { canvasColors, getTheme } from '../core/theme';
import { percentileApprox } from '../dsp/stats';
import { wfContrast, wfFloorSource, wfHiDbm, wfLoDbm, wfPalette, wfPaused, wfRangeMode } from '../ui/waterfallState';
import { getCapture, getView, isZoomed, viewportVersion } from '../ui/spectrumViewport';
import { previewFreqWindow } from '../ui/axisDrag';
import { lowerBound, projectSpectrum, sameFrequencyGrid } from '../dsp/grid';
import { sdrBadgeDragFrequency, sdrSpectrumWindow } from '../ui/sdrBadgeGesture';
import { demodBandHz, sdrDemod, sdrListenHz, sdrPitchHz } from '../ui/sdrState';
import { filterPreviewHz } from '../ui/sdrFilter';
import { filterHandlesVisible, passbandDragFrequency, renderFilterHandles } from '../ui/sdrFilterGesture';
import { sdrPassbandVisible } from '../ui/sdrPassbandState';
import { displayOffset } from '../ui/displayState';
import { waterfallLUT } from './waterfallPalette';
import { chooseViewFloor, observedFloor, WaterfallAutoLevels } from '../dsp/waterfallLevels';
import { requestRender } from './redraw';

const MAX_HISTORY_COLUMNS = 16384;
const MAX_SDR_COLUMNS = 65536;
type UniformRowGrid = { first: number; step: number; length: number };
type RowGeometry = {
	freq: Float64Array | null; lo: number; hi: number;
	/** Peak-projected columns are samples at cell centres, not capture endpoints. */
	uniform?: UniformRowGrid;
	/** SDR stores raw amplitude codes, with zero reserved for missing observations. */
	level?: { lo: number; step: number; windowLo: number; windowHi: number };
};
const rowGeometry = new WeakMap<Uint16Array, RowGeometry>();
let historyFreq: Float64Array | null = null;
let historyScratch = new Float32Array(0);
// One current display mapping per retained axis, never a cache of zoom histories.
const rowMappings = new WeakMap<Float64Array, { w: number; lo: number; hi: number; first: Int32Array; end: Int32Array }>();
type LevelMapping = {
	w: number; lo: number; hi: number; capLo: number; capHi: number;
	first: Int32Array; end: Int32Array; mix: Float32Array;
};
// Shared by all rows on an axis; zoom replaces its mapping rather than retaining views.
const levelMappings = new WeakMap<Float64Array | UniformRowGrid, LevelMapping>();
const mappingOrder: { key: WeakRef<Float64Array | UniformRowGrid>; mapping: LevelMapping }[] = [];
let mappingBytes = 0;
const uniformGrids = new WeakMap<Float64Array, UniformRowGrid | null>();
let floorKey = '';
let floorRow: Uint16Array | undefined;
let floorTarget = NaN;
let viewFloor = NaN;
let viewFloorAt = 0;
let levelFrame = 0;
let viewScaleMoving = false;
let viewWindow = { lo: -126, hi: -71, offset: false };

/** Most recent retained observation covering this view. Older axes are useful
 * after a paused retune back; no live spectrum can modify this calculation. */
export function retainedViewFloor(lo: number, hi: number): number {
  for (let r = S.waterfallRows.length - 1; r >= 0; r--) {
    const row = S.waterfallRows[r], g = rowGeometry.get(row);
    if (!g?.level || !g.freq || hi < g.lo || lo > g.hi) continue;
    const first = lowerBound(g.freq, Math.max(lo, g.lo));
    const stop = lowerBound(g.freq, Math.min(hi, g.hi));
    const end = stop < row.length && g.freq[stop] <= hi && g.freq[stop] <= g.hi ? stop + 1 : stop;
    const decode = (i: number) => row[i] ? g.level!.lo + (row[i] - 1) * g.level!.step : NaN;
    const local = observedFloor(Math.max(0, end - first), i => decode(i + first));
    if (!local.count) continue;
    const capture = observedFloor(row.length, i => g.freq![i] >= g.lo && g.freq![i] <= g.hi ? decode(i) : NaN, 0.3);
    const step = (g.freq[g.freq.length - 1] - g.freq[0]) / (g.freq.length - 1);
    return chooseViewFloor(local, Math.max(end - first, (hi - lo) / step), capture.floor);
  }
  return NaN;
}

function currentViewWindow(lo: number, hi: number) {
  const row = S.waterfallRows.at(-1);
  const key = `${lo}:${hi}:${wfPaused.get()}`;
  const now = performance.now();
  if (key !== floorKey || row !== floorRow) {
    const reset = S.waterfallRows.length === 1 && row !== floorRow;
    floorTarget = retainedViewFloor(lo, hi);
    if (reset || !Number.isFinite(viewFloor)) viewFloor = floorTarget;
    // Start a new transition at this interaction, even after a long idle pause.
    if (key !== floorKey) viewFloorAt = now;
    floorKey = key; floorRow = row;
  }
  // This clock animates colour limits only. Paused targets depend exclusively
  // on retained observations; arriving live frames cannot change the target.
  // Snap the tiny tail so a returned view eventually restores identical pixels.
  viewScaleMoving = false;
  if (Number.isFinite(floorTarget)) {
    const dt = Math.max(0, Math.min(250, now - viewFloorAt));
    viewFloor += (floorTarget - viewFloor) * (1 - Math.exp(-dt / 1500));
    if (Math.abs(floorTarget - viewFloor) < 0.025) viewFloor = floorTarget;
    else viewScaleMoving = true;
    const bottom = Math.round((viewFloor - 6) * 10) / 10;
    viewWindow = { lo: bottom, hi: bottom + 55, offset: false };
  }
  viewFloorAt = now;
  if (viewScaleMoving && !levelFrame && typeof requestAnimationFrame === 'function') {
    levelFrame = requestAnimationFrame(() => { levelFrame = 0; requestRender(); });
  }
  return viewWindow;
}

/** FFT axes are usually uniform. Retuning can retain hundreds of distinct axes;
 * use arithmetic lookup for those rather than binary-searching every pixel on
 * every old axis during a slider drag. Original Hz still correct rounding at
 * cell boundaries; nonuniform device grids keep the binary-search path.
 */
function nativeUniformGrid(freq: Float64Array): UniformRowGrid | undefined {
  const cached = uniformGrids.get(freq);
  if (cached !== undefined) return cached ?? undefined;
  const first = freq[0], step = (freq[freq.length - 1] - first) / (freq.length - 1);
  const tolerance = Math.max(1e-6, Math.abs(step) * 1e-7, Math.abs(first) * Number.EPSILON * 4);
  let valid = Number.isFinite(step) && step > 0;
  for (let i = 1; valid && i < freq.length; i++) valid = Math.abs(freq[i] - (first + i * step)) <= tolerance;
  const grid = valid ? { first, step, length: freq.length } : null;
  uniformGrids.set(freq, grid);
  return grid ?? undefined;
}

/** SDR codes are affine dBm samples: interpolate levels, then recolour retained history.
 * Pixels containing native samples keep their MAX; empty pixels interpolate only
 * between observed neighbours, never across zero codes or beyond source centres. */
function sliceLevelRow(row: Uint16Array, geometry: RowGeometry, w: number, lo: number, hi: number,
	out?: Uint16Array): Uint16Array {
	const dst = out && out.length === w ? out : new Uint16Array(w);
	const { freq, uniform, lo: capLo, hi: capHi } = geometry;
	const grid = freq ?? uniform!;
	let mapping = levelMappings.get(grid);
	if (!mapping || mapping.w !== w || mapping.lo !== lo || mapping.hi !== hi
		|| mapping.capLo !== capLo || mapping.capHi !== capHi) {
		const first = new Int32Array(w), end = new Int32Array(w), mix = new Float32Array(w);
		const at = (i: number) => freq ? freq[i] : uniform!.first + i * uniform!.step;
		const lower = (hz: number) => {
			if (!uniform) return lowerBound(freq!, hz);
			let i = Math.max(0, Math.min(row.length, Math.ceil((hz - uniform.first) / uniform.step)));
			if (freq) {
				while (i > 0 && freq[i - 1] >= hz) i--;
				while (i < row.length && freq[i] < hz) i++;
			}
			return i;
		};
		for (let x = 0; x < w; x++) {
			const a = lo + x / w * (hi - lo), b = lo + (x + 1) / w * (hi - lo);
			const start = Math.max(a, capLo), stop = Math.min(b, capHi);
			if (stop < start || b <= capLo || a > capHi) continue;
			first[x] = lower(start);
			end[x] = stop >= at(row.length - 1) ? row.length : lower(stop);
			// A pixel containing native samples keeps their MAX, including narrow carriers.
			if (first[x] < end[x]) continue;
			const mid = (a + b) / 2;
			if (mid < capLo || mid > capHi || mid < at(0) || mid > at(row.length - 1)) continue;
			const right = lower(mid), left = Math.max(0, right - 1);
			if (right >= row.length || right === left) continue;
			first[x] = left;
			end[x] = -1;
			mix[x] = (mid - at(left)) / (at(right) - at(left));
		}
		mapping = { w, lo, hi, capLo, capHi, first, end, mix };
    levelMappings.set(grid, mapping);
    mappingOrder.push({ key: new WeakRef(grid), mapping });
    mappingBytes += w * 12;
    while (mappingBytes > 8 * 1024 * 1024 && mappingOrder.length > 1) {
      const old = mappingOrder.shift()!;
      mappingBytes -= old.mapping.w * 12;
      const key = old.key.deref();
      if (key && levelMappings.get(key) === old.mapping) levelMappings.delete(key);
    }
	}
	for (let x = 0; x < w; x++) {
		const first = mapping.first[x], end = mapping.end[x];
		if (end === -1) {
			const a = row[first], b = row[first + 1];
			// Zero is missing, not a low level: neither side of a NaN may bridge it.
			dst[x] = a && b ? Math.round(a + (b - a) * mapping.mix[x]) : 0;
		} else {
			let peak = 0;
			for (let i = first; i < end; i++) if (row[i] > peak) peak = row[i];
			dst[x] = peak;
		}
	}
	return dst;
}

/** SDR rows retain native Hz; over-budget SWP/RTA rows use projected columns. */
export function sliceWaterfallRow(row: Uint16Array, w: number, lo: number, hi: number,
	out?: Uint16Array): Uint16Array {
	const geometry = rowGeometry.get(row);
	if (!geometry) return sliceDensityRow(row, w, 0, 1, out);
	if (geometry.level) return sliceLevelRow(row, geometry, w, lo, hi, out);
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

// 从原始行(任意宽度)降采样到瀑布显示宽度(复用 scratch 缓冲区，避免每行分配)

// Reused scratch state: the waterfall redraws on every render tick while rows arrive much
// more slowly, so buffers are kept instead of allocating an ImageData per frame.
let wfImg: ImageData | null = null;
let wfPixels: Uint32Array | null = null;
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
  const frequency = S.sdrMode && sdrPassbandVisible.get()
    ? sdrBadgeDragFrequency() ?? passbandDragFrequency() ?? sdrListenHz.get() : null;
  const view = sdrSpectrumWindow();
  if (view) renderSdrChannelBands(ctx, view.lo, view.hi, { x: 0, y: 0, w: width, h: height });
  const selection = getMarqueePreview();
  if (S.sdrMode && view && selection) {
    const x = (selection.lo - view.lo) / (view.hi - view.lo) * width;
    const w = (selection.hi - selection.lo) / (view.hi - view.lo) * width;
    ctx.save(); ctx.beginPath(); ctx.rect(0, 0, width, height); ctx.clip();
    ctx.fillStyle = 'rgba(0,229,255,0.08)'; ctx.fillRect(x, 0, w, height);
    ctx.strokeStyle = canvasColors().text; ctx.lineWidth = 1; ctx.setLineDash([5, 4]);
    ctx.strokeRect(x, 0, w, height); ctx.restore();
  }
  wfOverlayX = frequency !== null && view && frequency >= view.lo && frequency <= view.hi
    ? Math.max(0, Math.min(width - 1, Math.round((frequency - view.lo) / (view.hi - view.lo) * width))) : null;
  if (frequency === null || !view) return;
  const [bandLo, bandHi] = demodBandHz(sdrDemod.get(), filterPreviewHz(), sdrPitchHz.get());
  const scale = width / (view.hi - view.lo);
  const left = (frequency + bandLo - view.lo) * scale;
  const right = (frequency + bandHi - view.lo) * scale;
  ctx.save();
  const colors = canvasColors();
  drawPassband(ctx, left, right, 0, 0, width, height, colors, true);
  ctx.globalAlpha = 1;
  ctx.fillStyle = colors.rta;
  if (wfOverlayX !== null) ctx.fillRect(wfOverlayX, 0, Math.min(2, width - wfOverlayX), height);
  ctx.restore();
  // Arrows use the same CSS-pixel hit geometry and bandwidth gesture as the spectrum.
  if (filterHandlesVisible()) {
    const ratio = S.pixelRatio;
    ctx.save(); ctx.scale(ratio, ratio);
    renderFilterHandles(ctx, { plot: { x: 0, y: 0, w: width / ratio, h: height / ratio }, width: width / ratio, view });
    ctx.restore();
  }
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
export function renderWaterfall(canvas: HTMLCanvasElement, maxDensity: number, rowPixels = 1) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const W = canvas.width, H = canvas.height;
  // One observation occupies about one CSS pixel on every display. Repeat exact
  // rows on HiDPI; interpolating time would smear CW dots and burst boundaries.
  const rowHeight = Math.max(1, Math.round(rowPixels));
  const visibleRows = Math.ceil(H / rowHeight);
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
  const view = previewFreqWindow() ?? (S.sdrMode ? sdrSpectrumWindow() ?? cap : zv && cap
    && zv.lo >= cap.lo - 0.5 && zv.hi <= cap.hi + 0.5 ? zv : cap);
  let f0 = 0, f1 = 1;
  if (view && cap && cap.hi > cap.lo) {
    f0 = (view.lo - cap.lo) / (cap.hi - cap.lo);
    f1 = (view.hi - cap.lo) / (cap.hi - cap.lo);
  }
  const viewKey = `${view?.lo}:${view?.hi}:${f0.toFixed(4)}:${f1.toFixed(4)}:${viewportVersion()}`;
  const localWindow = S.sdrMode && wfRangeMode.get() === 'auto' && wfFloorSource.get() === 'view' && view
    ? currentViewWindow(view.lo, view.hi) : null;
  if (canvas.dataset) {
    const diagnostic = JSON.stringify({ lo: view?.lo, hi: view?.hi, rows: n, bytes: S.waterfallBytes,
      source: wfFloorSource.get(), floor: localWindow ? localWindow.lo + 6 : null,
      settled: !localWindow || !viewScaleMoving, newestColumns: rows[n - 1].length });
    if (canvas.dataset.history !== diagnostic) canvas.dataset.history = diagnostic;
  }
  const boundsKey = S.sdrMode ? `${displayOffset.get()}` : '';
  const imageKey = `${W}x${H}:${rowHeight}:${wfRangeMode.get()}:${localWindow?.lo}:${wfLoDbm.get()}:${wfHiDbm.get()}:${wfPalette.get()}:${wfContrast.get()}:${viewKey}:${boundsKey}`;
  const sameImage = wfCanvas === canvas && wfImg !== null && wfImgKey === imageKey && wfLastMax === dmax && wfLastTheme === theme;
  const pushed = S.waterfallPushes - wfLastPushes;
  if (sameImage && pushed === 0 && wfNewestRow === rows[n - 1]) {
    // Frozen history still has to erase the previous indicator before drawing the next.
    if (S.sdrMode || wfOverlayX !== null) {
      ctx.putImageData(wfImg!, 0, 0);
      paintBadgeIndicator(ctx, W, H);
    }
    return;
  }
  // Existing pixels already represent immutable historical rows. Scroll those pixels
  // once and rasterize only arrivals; zoom/resize/palette/reset rebuilds all rows.
  const scroll = sameImage && pushed > 0 && pushed < Math.min(n, visibleRows)
    && rows[n - 1 - pushed] === wfNewestRow ? pushed * rowHeight : 0;
  wfLastPushes = S.waterfallPushes;
  wfLastMax = dmax;
  wfLastTheme = theme;
  wfImgKey = imageKey;
  wfCanvas = canvas;
  wfNewestRow = rows[n - 1];
  if (!wfImg || wfImg.width !== W || wfImg.height !== H) {
    wfImg = ctx.createImageData(W, H);
    wfPixels = new Uint32Array(wfImg.data.buffer, wfImg.data.byteOffset, W * H);
  }
  const img = wfImg;
  const pixels = wfPixels!;
  if (scroll) pixels.copyWithin(scroll * W, 0, (H - scroll) * W);
  const lut = waterfallLUT(wfPalette.get(), wfContrast.get());
  const fixedBounds = wfRangeMode.get() === 'fixed';
  const fixedBottom = wfLoDbm.get() - (S.sdrMode ? displayOffset.get() : 0);
  const fixedTop = wfHiDbm.get() - (S.sdrMode ? displayOffset.get() : 0);
  const bg = canvasColors().bg;
  const bgPixel = (255 << 24) | (parseInt(bg.slice(5, 7), 16) << 16)
    | (parseInt(bg.slice(3, 5), 16) << 8) | parseInt(bg.slice(1, 3), 16);
  for (let y = 0; y < (scroll || H); y++) {
    // Newest row at TOP, older rows scroll down (top-down growth)
    const row = rows[n - 1 - Math.floor(y / rowHeight)];
    if (y % rowHeight && row) {
      const start = y * W;
      pixels.copyWithin(start, start - W, start);
      continue;
    }
    if (!row) {
      pixels.fill(bgPixel, y * W, (y + 1) * W);
      continue;
    }
    if (!sliceScratch || sliceScratch.length !== W) sliceScratch = new Uint16Array(W);
    const ds = view ? sliceWaterfallRow(row, W, view.lo, view.hi, sliceScratch)
      : sliceDensityRow(row, W, f0, f1, sliceScratch);
    const level = rowGeometry.get(row)?.level;
    const top = fixedBounds ? fixedTop : localWindow?.hi ?? level?.windowHi ?? 0;
    const bottom = fixedBounds ? fixedBottom : localWindow?.lo ?? level?.windowLo ?? 0;
    const lastColour = lut.length - 1;
    const scale = level ? level.step / (top - bottom) * lastColour : lastColour / dmax;
    const bias = level ? (level.lo - level.step - bottom) / (top - bottom) * lastColour : 0;
    const rowStart = y * W;
    for (let x = 0; x < W; x++) {
      const colour = ds[x] === 0 ? 0 : Math.max(0, Math.min(lastColour, Math.round(ds[x] * scale + bias)));
      pixels[rowStart + x] = level && ds[x] === 0 ? bgPixel : lut[colour];
    }
  }
  ctx.putImageData(img, 0, 0);
  paintBadgeIndicator(ctx, W, H);
}

// SWP/RTA density rows are mapped at ingestion. SDR stores native amplitude
// codes instead, retaining the automatic window alongside each observation.
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

const autoLevels = new WaterfallAutoLevels();
let autoCapture: { freq: Float64Array; lo: number; hi: number } | null = null;

/** Resolve the dB window independently of the main plot's vertical scale. */
function windowFor(levels: ArrayLike<number>, freq: Float64Array, capLo: number, capHi: number): { lo: number; hi: number; offset: boolean } {
  // Always retain the automatic window with SDR observations, even when Fixed
  // is selected, so switching back to Auto can recolour the complete history.
  if (S.sdrMode) {
    const reset = !S.waterfallRows.length || !autoCapture || autoCapture.lo !== capLo || autoCapture.hi !== capHi
      || !sameFrequencyGrid(autoCapture.freq, freq);
    if (reset) autoCapture = { freq, lo: capLo, hi: capHi };
    return autoLevels.update(levels, performance.now(), reset);
  }
  autoCapture = null;
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

/** SDR retains every supported native bin within the shared byte/row budget.
 * Oversized SWP/RTA grids retain their existing 16384-column peak projection. */
export function pushSpectrumRow(levels: Float32Array, freq: Float64Array, lo: number, hi: number, maxDensity: number) {
  // Reject unsupported SDR data instead of silently discarding its fine bins.
  if (S.sdrMode && levels.length > MAX_SDR_COLUMNS) return;
  const win = windowFor(levels, freq, lo, hi);
  let source = levels;
  let axis: Float64Array | null = null;
  if (levels.length <= (S.sdrMode ? MAX_SDR_COLUMNS : MAX_HISTORY_COLUMNS)) {
    if (!sameFrequencyGrid(historyFreq, freq)) historyFreq = freq;
    axis = historyFreq;
  } else {
    if (historyScratch.length !== MAX_HISTORY_COLUMNS) historyScratch = new Float32Array(MAX_HISTORY_COLUMNS);
    source = projectSpectrum(freq, levels, lo, hi, historyScratch);
  }
  let row: Uint16Array;
  const geometry: RowGeometry = { freq: axis, lo, hi };
  if (axis && S.sdrMode) geometry.uniform = nativeUniformGrid(axis);
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
    geometry.level = { lo: min, step, windowLo: win.lo, windowHi: win.hi };
  } else {
    row = buildRow(source, maxDensity, win.lo, win.hi, win.offset);
  }
  rowGeometry.set(row, geometry);
  S.pushWaterfallRow(row, axis);
}
