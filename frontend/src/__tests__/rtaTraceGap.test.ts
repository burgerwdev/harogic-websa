/**
 * The RTA/SDR trace polyline must break at a bin that carries no data.
 *
 * Native bins retain their measured Hz; coverage outside the capture stays empty.
 * A NaN native bin must lift the pen rather than drawing down to the plot boundary.
 *
 * The RTA renderer inked the pen across that sample. `getY()` puts a non-finite level below the
 * graticule. The trace then ran down the right plot edge for most of the plot height. The
 * operator reported that line as a signal. The swept renderer already lifted the pen. Both paths
 * now use `plottableLevel`.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ctxCalls, ctxImages, resetCtxCalls } from './setup';
import { renderAll } from '../render/spectrum';
import { getY, plotRect } from '../render/plot';
import * as S from '../core/store';
import { setDisplayRef } from '../ui/displayRef';
import { displayOffset } from '../ui/displayState';
import { axisDragMove, axisDragStart, resetAxisPreview } from '../ui/axisDrag';
import { __testSeedCapture, setView } from '../ui/spectrumViewport';
import { advanceDensity } from '../dsp/rtaDensity';
import { rtaDensContrast, rtaDensWidth, rtaDensLayers, rtaDensSource, rtaHalflife, waterfallOn } from '../ui/waterfallState';

/** The live SDR window: 23 MHz centre, decimate 128 (span 390.625 kHz). */
const LO = 22804687.5;
const HI = 23195312.5;
const PTS = 2048;

function sdrFrame(lastBinIsGap: boolean) {
  const freq = new Float64Array(PTS);
  const spec = new Float32Array(PTS);
  for (let i = 0; i < PTS; i++) {
    freq[i] = LO + (i * (HI - LO)) / (PTS - 1);
    spec[i] = -113 + (i % 5);            // a noise trace inside the window
  }
  if (lastBinIsGap) spec[PTS - 1] = NaN;
  S.setFreqArray(freq);
  S.setRtaData({ ver: 0, freq, spec, wfRow: new Uint16Array(0), maxDensity: 0, startHz: LO, stopHz: HI });
  S.setRtaDisplays([spec, null, null, null]);
}

beforeEach(() => {
  resetCtxCalls();
  setDisplayRef('preset', -80);
  S.setDbPerDiv(10);
  S.setFreqArray(null);
  S.setRtaDisplays([null, null, null, null]);
  S.setRtaData(null);
  S.setRtaDensity2d(null);
  S.setViewMode('rta');
  S.setRtaMode(true);
});

describe('renderRta', () => {
  it('draws nothing below the graticule when the last bin has no data', () => {
    sdrFrame(true);
    renderAll();
    const p = plotRect();
    const below = ctxCalls.filter((c) => c.y > p.y + p.h + 0.5);
    expect(below).toEqual([]);
    // The trace itself is still drawn, up to the last bin that has data.
    const line = ctxCalls.filter((c) => c.op === 'lineTo');
    expect(line.length).toBeGreaterThan(PTS / 2);
    expect(Math.max(...line.map((c) => c.x))).toBeGreaterThan(p.x + p.w - 2);
  });

  it('draws the last bin when the frame has no gap there', () => {
    sdrFrame(false);
    renderAll();
    const p = plotRect();
    // A complete frame keeps the invariant above honest. The last bin is drawn, and it sits
    // inside the graticule at the right edge.
    const line = ctxCalls.filter((c) => c.op === 'lineTo');
    const last = line[line.length - 1];
    expect(last.x).toBeCloseTo(p.x + p.w, 0);
    expect(last.y).toBeLessThan(p.y + p.h);
  });

  it('keeps native trace levels aligned with Ref, offset and a live level preview', () => {
    sdrFrame(false);
    S.rtaData.spec[1023] = -20;
    const savedOffset = displayOffset.get();
    const p = plotRect();
    try {
      setDisplayRef('preset', -60);
      displayOffset.set(7);
      axisDragStart('y', p.x + p.w + 20, p.y + 50, p);
      axisDragMove(p.x + p.w + 20, p.y + 80);
      resetCtxCalls();
      renderAll();
      const index = 1023;
      const x = p.x + index / (PTS - 1) * p.w;
      const point = ctxCalls.find(call => call.op === 'lineTo' && Math.abs(call.x - x) < 1e-7)!;
      expect(point.y).toBeCloseTo(getY(S.rtaData.spec[index]), 8);
    } finally {
      resetAxisPreview();
      displayOffset.set(savedOffset);
    }
  });

  it('keeps original extrema, final carrier and an internal gap when native bins share pixels', () => {
    const p = plotRect();
    const freq = Float64Array.from({ length: 16240 }, (_, i) => LO + i / 16239 * (HI - LO));
    const spec = new Float32Array(freq.length).fill(-100);
    spec[8001] = -25;
    spec[8002] = -140;
    spec[8003] = -30;
    spec[8004] = NaN;
    spec[spec.length - 1] = -35;
    S.setFreqArray(freq);
    S.setRtaData({ ver: 0, freq, spec, wfRow: new Uint16Array(0), maxDensity: 0, startHz: LO, stopHz: HI });
    S.setRtaDisplays([spec, null, null, null]);
    renderAll();
    const xAt = (i: number) => p.x + (freq[i] - LO) / (HI - LO) * p.w;
    for (const index of [8001, 8002, spec.length - 1]) {
      const point = ctxCalls.find(call => Math.abs(call.x - xAt(index)) < 1e-7 && Math.abs(call.y - getY(spec[index])) < 1e-7);
      expect(point).toBeDefined();
    }
    expect(ctxCalls.some(call => call.op === 'moveTo' && Math.abs(call.x - xAt(8005)) < 1e-7)).toBe(true);
    __testSeedCapture({ lo: LO, hi: HI });
    try {
      setView({ lo: freq[7999], hi: freq[8006] }, 2 * (freq[1] - freq[0]));
      resetCtxCalls();
      renderAll();
      for (const index of [8001, 8002, 8003]) {
        const x = p.x + (freq[index] - freq[7999]) / (freq[8006] - freq[7999]) * p.w;
        expect(ctxCalls.some(call => Math.abs(call.x - x) < 1e-7 && Math.abs(call.y - getY(spec[index])) < 1e-7)).toBe(true);
      }
    } finally {
      __testSeedCapture(null);
    }
  });

  it('moves the existing density with the trace for paused offset/ref/scale and axis preview', () => {
    let image: ImageData;
    const offscreen = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: (value: ImageData) => { image = value; },
    };
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(offscreen as unknown as CanvasRenderingContext2D);
    const savedOffset = displayOffset.get();
    const savedFade = rtaHalflife.get();
    const savedWaterfall = waterfallOn.get();
    const savedWidth = rtaDensWidth.get(), savedContrast = rtaDensContrast.get();
    const savedTheme = document.documentElement.dataset.theme;
    const p = plotRect();
    try {
      S.traces[0].mode = 'CLEAR_WRITE';
      rtaHalflife.set(999); waterfallOn.set(false);
      displayOffset.set(0);
      setDisplayRef('preset', -20);
      sdrFrame(false);
      S.rtaData.spec[1023] = -60;
      const density = advanceDensity(null, S.rtaDisplays[0], 128, 999, -20, 1)!;
      const values = density.values.slice(), samples = density.samples.slice();
      S.setRtaDensity2d(density);
      const assertAligned = () => {
        resetCtxCalls(); renderAll();
        const draw = ctxImages[0];
        expect(draw).toBeDefined();
        const [left, top, width, height] = draw.args;
        const x = Math.floor(1023 / (PTS - 1) * image.width);
        const expectedY = getY(-60);
        let mass = 0, weighted = 0;
        for (let y = 0; y < image.height; y++) {
          const screenY = top + (y + 0.5) / image.height * height;
          if (Math.abs(screenY - expectedY) > 12) continue;
          const alpha = image.data[(y * image.width + x) * 4 + 3];
          mass += alpha; weighted += alpha * screenY;
        }
        expect(mass).toBeGreaterThan(0);
        expect(Math.abs(weighted / mass - expectedY)).toBeLessThanOrEqual(height / image.height / 2 + 0.01);
        expect(left).toBe(p.x); expect(width).toBe(p.w);
      };
      assertAligned();
      for (const theme of ['light', 'dark']) for (const width of [1, 9]) {
        document.documentElement.dataset.theme = theme;
        rtaDensWidth.set(width); rtaDensContrast.set(0.6);
        assertAligned();
      }
      displayOffset.set(7); setDisplayRef('preset', -30); S.setDbPerDiv(7);
      assertAligned();
      axisDragStart('y', p.x + p.w + 20, p.y + 50, p);
      axisDragMove(p.x + p.w + 20, p.y + 80);
      assertAligned();
      expect(density.values).toEqual(values); expect(density.samples).toEqual(samples);
    } finally {
      canvas.mockRestore();
      resetAxisPreview();
      S.setRtaDensity2d(null);
      displayOffset.set(savedOffset); rtaHalflife.set(savedFade); waterfallOn.set(savedWaterfall);
      rtaDensWidth.set(savedWidth); rtaDensContrast.set(savedContrast);
      if (savedTheme === undefined) delete document.documentElement.dataset.theme;
      else document.documentElement.dataset.theme = savedTheme;
    }
  });

  it('keeps live density visible when the active trace is OFF', () => {
    const offscreen = {
      createImageData: (width: number, height: number) => ({ width, height, data: new Uint8ClampedArray(width * height * 4) }),
      putImageData: () => {},
    };
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(offscreen as unknown as CanvasRenderingContext2D);
    try {
      sdrFrame(false);
      const density = advanceDensity(null, S.rtaDisplays[0], 128, 999, -80, 1, 'CLEAR_WRITE', S.rtaData.freq, 'live')!;
      S.setRtaDensity2d(density);
      S.traces[0].mode = 'OFF';
      rtaDensSource.set('live');
      rtaDensLayers.set('density');
      rtaHalflife.set(2);
      renderAll();
      expect(ctxImages.length).toBeGreaterThan(0);
      expect(document.getElementById('spectrum')!.dataset.density).toBe('on');
    } finally {
      canvas.mockRestore();
      rtaDensSource.set('trace'); rtaDensLayers.set('both');
    }
  });

  it('retains the trace while density-only history is being rebuilt', () => {
    sdrFrame(false);
    S.traces[0].mode = 'CLEAR_WRITE';
    rtaDensLayers.set('density');
    rtaHalflife.set(2);
    S.setRtaDensity2d(null);
    renderAll();
    expect(ctxCalls.filter((c) => c.op === 'lineTo').length).toBeGreaterThan(PTS / 2);
    expect(document.getElementById('spectrum')!.dataset.density).toBe('off');
    rtaDensLayers.set('both');
  });
});
