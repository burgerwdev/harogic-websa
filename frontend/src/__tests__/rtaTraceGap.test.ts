/**
 * The RTA/SDR trace polyline must break at a bin that carries no data.
 *
 * The SDR panadapter publishes its capture grid. That grid can stop before the display window
 * stops. `dsp/grid.ts` then leaves the outermost display bin at NaN. It does not invent a value.
 * Bench measurement at 23 MHz with decimate 128: one NaN, the last bin of 2048.
 *
 * The RTA renderer inked the pen across that sample. `getY()` puts a non-finite level below the
 * graticule. The trace then ran down the right plot edge for most of the plot height. The
 * operator reported that line as a signal. The swept renderer already lifted the pen. Both paths
 * now use `plottableLevel`.
 */
import { beforeEach, describe, expect, it } from 'vitest';

import { ctxCalls, resetCtxCalls } from './setup';
import { renderAll } from '../render/spectrum';
import { plotRect } from '../render/plot';
import * as S from '../core/store';
import { setDisplayRef } from '../ui/displayRef';

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
});
