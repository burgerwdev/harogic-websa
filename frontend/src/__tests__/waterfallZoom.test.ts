import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { pushSpectrumRow, renderWaterfall, retainedViewFloor, sliceDensityRow, sliceWaterfallRow } from '../render/waterfall';
import * as S from '../core/store';
import { wfContrast, wfFloorSource, wfPaused, wfPalette, wfRangeMode, wfLoDbm, wfHiDbm } from '../ui/waterfallState';
import { __testSeedCapture, zoomEnabled } from '../ui/spectrumViewport';
import { clearSdrBadgeDrag, sdrBadgeGeometry, startSdrBadgeDrag } from '../ui/sdrBadgeGesture';
import { sdrListenHz } from '../ui/sdrState';
import { setTheme } from '../core/theme';
import { setSdrHoverFrequency } from '../ui/sdrHoverPassband';
import { clearDisplayBounds, setDisplayBounds } from '../ui/displayRef';

beforeEach(() => {
  S.resetWaterfall();
  wfRangeMode.set('fixed');
  wfHiDbm.set(0);
  wfLoDbm.set(-100);
  wfPalette.set('classic');
  wfContrast.set(1);
  wfFloorSource.set('capture'); wfPaused.set(false);
});

afterEach(() => {
  clearSdrBadgeDrag();
  setSdrHoverFrequency(null);
  clearDisplayBounds();
  zoomEnabled.set(false);
  S.setSdrMode(false);
  S.setRtaData(null);
  __testSeedCapture(null);
  setTheme('dark');
  S.setFrequencyLimits(9000, 9e9);
});

describe('native waterfall history', () => {
  it('estimates on original RF samples and can browse frozen old axes without live input', () => {
    S.setSdrMode(true); wfPaused.set(true);
    const freq = Float64Array.from({ length: 1000 }, (_, i) => 1e6 + i);
    const levels = new Float32Array(1000).fill(-90);
    levels.fill(-120, 100, 200); levels[150] = -10; levels[170] = NaN;
    pushSpectrumRow(levels, freq, 1e6, 1e6 + 999, 100);
    const original = S.waterfallRows[0].slice();
    expect(retainedViewFloor(1e6 + 100, 1e6 + 199)).toBeCloseTo(-120, 0);
    expect(retainedViewFloor(1e6 + 150, 1e6 + 152)).toBeCloseTo(-90, 0);
    pushSpectrumRow(new Float32Array(1000).fill(-80), Float64Array.from(freq, v => v + 2000), 1e6 + 2000, 1e6 + 2999, 100);
    expect(retainedViewFloor(1e6 + 100, 1e6 + 199)).toBeCloseTo(-120, 0);
    expect(retainedViewFloor(2e6, 2e6 + 1000)).toBeNaN();
    expect(S.waterfallRows[0]).toEqual(original);
  });

  it('keeps the current passband after a badge drag on an empty waterfall', () => {
    S.initStore();
    S.setSdrMode(true);
    const freq = Float64Array.from([10_000, 20_000]);
    S.setRtaData({ freq, startHz: 10_000, stopHz: 20_000 });
    __testSeedCapture({ lo: 10_000, hi: 20_000 });
    sdrListenHz.confirm(15_000);
    const context = { fillStyle: '', fillRect: vi.fn(), save: vi.fn(), restore: vi.fn(), beginPath: vi.fn(), rect: vi.fn(), clip: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), setLineDash: vi.fn() };
    const canvas = { width: 100, height: 40, getContext: () => context } as unknown as HTMLCanvasElement;
    const g = sdrBadgeGeometry()!;
    startSdrBadgeDrag(g.left, g, clearSdrBadgeDrag);
    renderWaterfall(canvas, 100);
    expect(context.fillRect).toHaveBeenLastCalledWith(50, 0, 2, 40);
    clearSdrBadgeDrag();
    context.fillRect.mockClear();
    renderWaterfall(canvas, 100);
    expect(context.fillRect).toHaveBeenNthCalledWith(1, 0, 0, 100, 40);
    expect(context.fillRect).toHaveBeenLastCalledWith(50, 0, 2, 40);
  });

  it('retains a close pair, valley, missing bin and final carrier on their native Hz', () => {
    const freq = Float64Array.from([10, 12, 14, 16, 18]);
    pushSpectrumRow(Float32Array.from([-20, -95, -30, NaN, -25]), freq, 8, 20, 100);
    const row = S.waterfallRows[0];
    expect([...row]).toEqual([80, 5, 70, 0, 75]);
    expect([...sliceWaterfallRow(row, 3, 10, 16)]).toEqual([80, 5, 70]);
    expect([...sliceWaterfallRow(row, 4, 15, 19)]).toEqual([0, 0, 75, 75]);
    expect([...sliceWaterfallRow(row, 3, 8, 11)]).toEqual([0, 0, 80]);
  });

  it('recolours retained SDR levels with the waterfall range and keeps sub-dB carriers distinct', () => {
    S.setSdrMode(true);
    zoomEnabled.set(true);
    const freq = Float64Array.from([10, 12, 14, 16]);
    __testSeedCapture({ lo: 10, hi: 16 });
    let pixels = new Uint8ClampedArray();
    const context = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: (image: { data: Uint8ClampedArray }) => { pixels = image.data.slice(); },
    };
    const canvas = { width: 4, height: 1, getContext: () => context } as unknown as HTMLCanvasElement;
    pushSpectrumRow(Float32Array.from([-60.8, -60.2, NaN, -20]), freq, 10, 16, 100);
    const row = S.waterfallRows[0];
    const pushes = S.waterfallPushes;
    wfHiDbm.set(-60); wfLoDbm.set(-61);
    renderWaterfall(canvas, 100);
    const narrow = pixels.slice();
    expect([...pixels.slice(0, 4)]).not.toEqual([...pixels.slice(4, 8)]);
    setDisplayBounds(0, -100);
    renderWaterfall(canvas, 100);
    expect(pixels).toEqual(narrow); // Spectrum scale cannot flatten waterfall contrast.
    wfHiDbm.set(0); wfLoDbm.set(-100);
    renderWaterfall(canvas, 100);
    expect(pixels).not.toEqual(narrow);
    expect(S.waterfallRows[0]).toBe(row);
    expect(S.waterfallPushes).toBe(pushes);
    wfHiDbm.set(-60); wfLoDbm.set(-61);
    renderWaterfall(canvas, 100);
    expect(pixels).toEqual(narrow);
    wfPalette.set('amber');
    renderWaterfall(canvas, 100);
    expect(pixels).not.toEqual(narrow);
    wfPalette.set('classic'); wfContrast.set(0.7);
    renderWaterfall(canvas, 100);
    expect(pixels).not.toEqual(narrow);
    wfContrast.set(1);
    renderWaterfall(canvas, 100);
    expect(pixels).toEqual(narrow);
    expect(S.waterfallRows[0]).toBe(row);
    expect(S.waterfallPushes).toBe(pushes);
  });

  it('preserves exact burst boundaries and scrolls by observation on HiDPI displays', () => {
    const freq = Float64Array.from([10, 12, 14, 16]);
    __testSeedCapture({ lo: 10, hi: 16 });
    let pixels = new Uint8ClampedArray();
    const context = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: (image: { data: Uint8ClampedArray }) => { pixels = image.data.slice(); },
    };
    const canvas = { width: 4, height: 6, getContext: () => context } as unknown as HTMLCanvasElement;
    pushSpectrumRow(Float32Array.from([-80, -60, -40, -20]), freq, 10, 16, 100);
    renderWaterfall(canvas, 100, 2);
    const first = pixels.slice(0, 16);
    expect(pixels.slice(16, 32)).toEqual(first);
    pushSpectrumRow(Float32Array.from([-20, -40, -60, -80]), freq, 10, 16, 100);
    renderWaterfall(canvas, 100, 2);
    expect(pixels.slice(0, 16)).not.toEqual(first);
    expect(pixels.slice(16, 32)).toEqual(pixels.slice(0, 16));
    expect(pixels.slice(32, 48)).toEqual(first);
    expect(pixels.slice(48, 64)).toEqual(first);
    const incremental = pixels.slice();
    canvas.width = 5; renderWaterfall(canvas, 100, 2);
    canvas.width = 4; renderWaterfall(canvas, 100, 2);
    expect(pixels).toEqual(incremental);
  });

  it('reconstructs SDR zoom slopes before colour mapping while retaining native peaks and gaps', () => {
    S.setSdrMode(true);
    const freq = Float64Array.from([10, 14, 18, 22, 26]);
    pushSpectrumRow(Float32Array.from([-100, -20, -80, NaN, -40]), freq, 8, 28, 100);
    const row = S.waterfallRows[0];
    const scratch = new Uint16Array(16);
    const slope = sliceWaterfallRow(row, 16, 10, 18, scratch);
    expect(slope).toBe(scratch);
    expect(slope[0]).toBe(row[0]);
    for (let x = 1; x < 8; x++) expect(slope[x]).toBeGreaterThan(slope[x - 1]);
    expect(slope[8]).toBe(row[1]); // The native carrier survives between pixel centres.
    expect([...sliceWaterfallRow(row, 2, 10, 26)]).toEqual([row[1], row[4]]);
    const gap = sliceWaterfallRow(row, 16, 18, 26);
    expect(gap[0]).toBe(row[2]);
    expect([...gap.slice(1, -1)]).toEqual(new Array(14).fill(0));
    expect(gap[15]).toBe(row[4]);
    expect([...sliceWaterfallRow(row, 4, 8, 10)]).toEqual([0, 0, 0, 0]);
    expect([...sliceWaterfallRow(row, 4, 26.5, 28)]).toEqual([0, 0, 0, 0]);
  });

  it('anchors historical signals in RF through repeated fractional pans, span changes and a return to the old band', () => {
    S.setSdrMode(true);
    const freq = Float64Array.from({ length: 1024 }, (_, i) => 7e6 + i * 10);
    const levels = new Float32Array(freq.length).fill(-110);
    levels[400] = -40; levels[402] = -50; levels[401] = -108;
    levels[600] = NaN;
    pushSpectrumRow(levels, freq, 7e6, 7e6 + 10240, 100);
    const original = S.waterfallRows[0], codes = original.slice();
    const view = sliceWaterfallRow(original, 1024, 7e6, 7e6 + 10240);
    for (let i = 0; i < 150; i++) {
      // Fractions smaller than one display column must not accumulate a shift error.
      sliceWaterfallRow(original, 1024, 7e6 + i * 0.7, 7e6 + 10240 + i * 0.7);
    }
    expect(original).toEqual(codes);
    expect(sliceWaterfallRow(original, 1024, 7e6, 7e6 + 10240)).toEqual(view);
    const shifted = sliceWaterfallRow(original, 1024, 7e6 + 2000, 7e6 + 12240);
    expect(shifted[200]).toBe(original[400]);
    expect(shifted[201]).toBe(original[401]);
    expect(shifted[202]).toBe(original[402]);
    expect([...shifted.slice(824)]).toEqual(new Array(200).fill(0));
    // A finer capture can share the history without changing its original resolution.
    const fineFreq = Float64Array.from({ length: 4096 }, (_, i) => 7e6 + 3000 + i);
    pushSpectrumRow(new Float32Array(4096).fill(-100), fineFreq, 7e6 + 3000, 7e6 + 7096, 100);
    expect(S.waterfallRows[0]).toBe(original);
    expect(sliceWaterfallRow(original, 4, 7e6 + 4000, 7e6 + 4040)).toEqual(original.slice(400, 404));
    expect([...sliceWaterfallRow(original, 8, 8e6, 8e6 + 10000)]).toEqual(new Array(8).fill(0));
  });

  it.each([false, true])('keeps exact pixel membership on a GHz grid (nonuniform=%s)', nonuniform => {
    S.setSdrMode(true);
    const freq = Float64Array.from({ length: 1024 }, (_, i) => 1e9 + i * 120000 / 1023);
    if (nonuniform) freq[400] += 20;
    const levels = Float32Array.from(freq, (_, i) => -110 + 30 * (0.5 + 0.5 * Math.sin(i * 2.1)));
    pushSpectrumRow(levels, freq, freq[0], freq[freq.length - 1] + 1, 100);
    const row = S.waterfallRows[0];
    for (const shift of [0, 0.7, 120, -120]) {
      const lo = freq[0] + shift, hi = lo + 120001;
      const projected = sliceWaterfallRow(row, 64, lo, hi);
      const reference = new Uint16Array(64);
      for (let i = 0; i < freq.length; i++) {
        if (freq[i] < lo || freq[i] > hi) continue;
        const pixel = Math.min(63, Math.floor((freq[i] - lo) / (hi - lo) * 64));
        reference[pixel] = Math.max(reference[pixel], row[i]);
      }
      expect(projected).toEqual(reference);
    }
  });

  it('retains every large SDR FFT bin including an adjacent valley', () => {
    S.setSdrMode(true);
    const freq = Float64Array.from({ length: 52429 }, (_, i) => 1e9 + i * 10);
    const levels = new Float32Array(freq.length).fill(-110);
    levels[40001] = -40; levels[40002] = -108; levels[40003] = -41;
    pushSpectrumRow(levels, freq, freq[0], freq.at(-1)!, 100);
    const row = S.waterfallRows[0];
    expect(row).toHaveLength(52429);
    expect(sliceWaterfallRow(row, 3, freq[40001], freq[40004])).toEqual(row.slice(40001, 40004));
    expect(row[40002]).toBeLessThan(row[40001]);
    expect(row[40002]).toBeLessThan(row[40003]);
    expect(S.waterfallBytes).toBe(row.byteLength + freq.byteLength);
  });

  it('shares identical RF axes and evicts complete old rows within the byte budget', () => {
    S.setSdrMode(true);
    const levels = new Float32Array(52429).fill(-100);
    const freq = Float64Array.from(levels, (_, i) => 1e9 + i);
    for (let i = 0; i < 2; i++) pushSpectrumRow(levels, i ? freq.slice() : freq, freq[0], freq.at(-1)!, 100);
    expect(S.waterfallBytes).toBe(2 * levels.length * 2 + freq.byteLength);
    // The store budget applies before rendering or any expensive encoding.
    for (let i = 2; i < 513; i++) S.pushWaterfallRow(new Uint16Array(levels.length), freq);
    expect(S.waterfallRows).toHaveLength(512);
    expect(S.waterfallBytes).toBe(512 * levels.length * 2 + freq.byteLength);
    for (let i = 0; i < 150; i++) {
      const shifted = freq.slice(); shifted[0] += i * 0.01;
      S.pushWaterfallRow(new Uint16Array(levels.length), shifted);
    }
    expect(S.waterfallBytes).toBeLessThanOrEqual(S.WATERFALL_HISTORY_BYTES);
    expect(S.waterfallRows.length).toBeLessThan(512);
    expect(S.waterfallRows.at(-1)).toHaveLength(52429);
  });

  it('bounds oversized history while retaining a carrier in the last native bin', () => {
    const freq = Float64Array.from({ length: 32769 }, (_, i) => i);
    const levels = new Float32Array(freq.length).fill(-100);
    levels[levels.length - 1] = -10;
    for (let i = 0; i < 513; i++) pushSpectrumRow(levels, freq, 0, 32768, 100);
    expect(S.waterfallRows).toHaveLength(512);
    expect(S.waterfallRows[511]).toHaveLength(16384);
    expect(sliceWaterfallRow(S.waterfallRows[511], 1, 32767, 32768)[0]).toBe(90);
  });

  it('scrolls new rows without changing historical pixels and rebuilds correctly after reset', () => {
    const freq = Float64Array.from([10, 12, 14, 16]);
    let pixels = new Uint8ClampedArray();
    const context = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: (image: { data: Uint8ClampedArray }) => { pixels = image.data.slice(); },
    };
    const canvas = { width: 4, height: 3, getContext: () => context } as unknown as HTMLCanvasElement;
    __testSeedCapture({ lo: 10, hi: 16 });
    try {
      pushSpectrumRow(Float32Array.from([-80, -60, -40, -20]), freq, 10, 16, 100);
      renderWaterfall(canvas, 100);
      const originalTop = pixels.slice(0, 16);
      pushSpectrumRow(Float32Array.from([-20, -40, -60, -80]), freq, 10, 16, 100);
      renderWaterfall(canvas, 100);
      expect([...pixels.slice(16, 32)]).toEqual([...originalTop]);
      const incremental = pixels.slice();
      const rows = [...S.waterfallRows];
      S.resetWaterfall();
      // A reset can replace rows without changing the final push counter.
      for (const row of rows) S.pushWaterfallRow(row);
      canvas.width = 5;
      renderWaterfall(canvas, 100);
      canvas.width = 4;
      renderWaterfall(canvas, 100);
      expect([...pixels]).toEqual([...incremental]);
    } finally {
      __testSeedCapture(null);
    }
  });
});

describe('sliceDensityRow', () => {
	it('is identity when the row width equals the canvas width', () => {
		const row = Uint16Array.from([1, 2, 3, 4]);
		const out = sliceDensityRow(row, 4, 0, 1);
		expect([...out]).toEqual([1, 2, 3, 4]);
	});

	it('aggregates with MAX on the way down', () => {
		const row = Uint16Array.from([1, 9, 2, 8]);
		expect([...sliceDensityRow(row, 2, 0, 1)]).toEqual([9, 8]);
	});

	it('upscales by nearest column, never interpolating', () => {
		const row = Uint16Array.from([7, 0, 0, 3]);
		const out = sliceDensityRow(row, 8, 0, 1);
		expect([...out]).toEqual([7, 7, 0, 0, 0, 0, 3, 3]);
	});

	it('slices a sub-range of the capture (the zoomed view)', () => {
		// 8 columns across the capture; the view is the right half.
		const row = Uint16Array.from([1, 2, 3, 4, 5, 6, 7, 8]);
		const out = sliceDensityRow(row, 4, 0.5, 1);
		expect([...out]).toEqual([5, 6, 7, 8]);
	});

	it('handles rows stored at different widths (a resize mid-session)', () => {
		const narrow = Uint16Array.from([5, 1]);
		const wide = Uint16Array.from([1, 1, 1, 1, 1, 1, 1, 6]);
		// Same view fraction over both: the peak (5 vs 6) must surface either way.
		expect(sliceDensityRow(narrow, 4, 0, 1).reduce((a, b) => Math.max(a, b))).toBe(5);
		expect(sliceDensityRow(wide, 4, 0, 1).reduce((a, b) => Math.max(a, b))).toBe(6);
	});

	it('reuses the caller scratch buffer when it fits', () => {
		const scratch = new Uint16Array(4);
		const out = sliceDensityRow(Uint16Array.from([1, 2, 3, 4]), 4, 0, 1, scratch);
		expect(out).toBe(scratch);
	});
});
