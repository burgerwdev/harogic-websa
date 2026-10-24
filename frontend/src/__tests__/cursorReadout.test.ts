/**
 * Cursor readout: the frequency and level label for the bin under the pointer.
 *
 * Two properties are important, and the numbers alone do not show them.
 *   1. The hit test selects the bin that the trace draws under the pointer. The test searches
 *      getX, so a display zoom or a gesture preview cannot move the readout off the pixels.
 *   2. The readout never covers the corner slot: the RTA/SDR mode badge, the 3 dB BW label and the
 *      harmonic labels. A hover never moves that slot either.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import '../meas/harmonic';      // registers the 'harm' view, as main.ts does
import '../meas/phaseNoise';    // registers the 'pnm' view
import { getX, getY, nearestIdxAtX, plotRect } from '../render/plot';
import { renderTraceLine, topLeftLines } from '../render/spectrum';
import { __testSeedCapture, setView } from '../ui/spectrumViewport';
import { spectrumFreqAtX } from '../ui/spectrumZoomGestures';
import { placeMarkerFromX } from '../ui/panels/markers';
import { getTraceDisplay } from '../dsp/peaks';
import { smoothBins } from '../ui/displayState';
import { ctxCalls, resetCtxCalls } from './setup';

beforeEach(() => {
	S.setViewMode('std');
	S.setM3dB(null);
});

describe('native Hz consumer geometry', () => {
  it('places fractional measurement coordinates between adjacent native bin frequencies', () => {
    const savedFreq = S.freqArray;
    S.setFreqArray(Float64Array.from([100, 102, 110, 120]));
    __testSeedCapture({ lo: 95, hi: 125 });
    try {
      const p = plotRect();
      expect(getX(1.5, 4)).toBeCloseTo(p.x + (106 - 95) / 30 * p.w, 8);
      expect(getX(-0.5, 4)).toBeCloseTo(getX(0, 4), 8);
      expect(getX(3.5, 4)).toBeCloseTo(getX(3, 4), 8);
    } finally {
      S.setFreqArray(savedFreq);
      __testSeedCapture(null);
    }
  });

  it('maps a full-window pointer to the drawn native Hz and marker despite partial coverage', () => {
    const savedFreq = S.freqArray;
    const marker = S.markers.find(m => m.id === S.activeMkrId)!;
    const savedMarker = { ...marker };
    const freq = Float64Array.from([102, 103, 117, 118]);
    S.setFreqArray(freq);
    __testSeedCapture({ lo: 100, hi: 120 });
    try {
      const x = getX(2, 4);
      expect(spectrumFreqAtX(x)).toBeCloseTo(117, 8);
      placeMarkerFromX(x, new Float32Array(4));
      expect(marker.idx).toBe(2);
      expect(nearestIdxAtX(x, 4)).toBe(2);
    } finally {
      Object.assign(marker, savedMarker);
      S.setFreqArray(savedFreq);
      __testSeedCapture(null);
    }
  });

  it('draws the same adaptive smoothed level as marker readouts before and after zoom', () => {
    const trace = S.traces[0];
    const saved = { ...trace }, savedFreq = S.freqArray, savedSmooth = smoothBins.get();
    const powers = new Float32Array(101).fill(-100);
    powers.set([-100, -80, -20, -100, -100], 48);
    trace.powers = powers;
    trace.mode = 'CLEAR_WRITE';
    S.setFreqArray(Float64Array.from({ length: 101 }, (_, i) => i));
    smoothBins.set(5);
    __testSeedCapture({ lo: 0, hi: 100 });
    try {
      const expected = getY(getTraceDisplay(trace)![50]);
      for (const view of [null, { lo: 49, hi: 51 }]) {
        setView(view);
        resetCtxCalls();
        renderTraceLine(trace);
        const point = ctxCalls.find(call => Math.abs(call.x - getX(50, 101)) < 1e-8)!;
        expect(point.y).toBeCloseTo(expected, 8);
      }
    } finally {
      Object.assign(trace, saved);
      S.setFreqArray(savedFreq);
      smoothBins.set(savedSmooth);
      __testSeedCapture(null);
    }
  });

  it('keeps swept native extrema and gaps through a bounded raster and reveals both peaks on zoom', () => {
    const trace = S.traces[0];
    const saved = { ...trace }, savedFreq = S.freqArray, savedSmooth = smoothBins.get();
    const powers = new Float32Array(16240).fill(-100);
    powers.set([-25, -140, -30, NaN], 8001);
    powers[16239] = -35;
    trace.powers = powers;
    trace.mode = 'CLEAR_WRITE';
    S.setFreqArray(Float64Array.from({ length: powers.length }, (_, i) => i));
    smoothBins.set(1);
    __testSeedCapture({ lo: 0, hi: 16239 });
    try {
      for (const view of [null, { lo: 7999, hi: 8006 }]) {
        setView(view, 2);
        resetCtxCalls();
        renderTraceLine(trace);
        const indices = view ? [8001, 8002, 8003] : [8001, 8002, 16239];
        for (const index of indices) {
          expect(ctxCalls.some(call => Math.abs(call.x - getX(index, powers.length)) < 1e-7
            && Math.abs(call.y - getY(powers[index])) < 1e-7)).toBe(true);
        }
        expect(ctxCalls.some(call => call.op === 'moveTo' && Math.abs(call.x - getX(8005, powers.length)) < 1e-7)).toBe(true);
      }
    } finally {
      Object.assign(trace, saved);
      S.setFreqArray(savedFreq);
      smoothBins.set(savedSmooth);
      __testSeedCapture(null);
    }
  });
});

describe('cursor hit test', () => {
	const n = 101;
	const p = plotRect();

	it('maps a canvas x to the drawn bin', () => {
		expect(nearestIdxAtX(p.x, n)).toBe(0);
		expect(nearestIdxAtX(p.x + p.w, n)).toBe(n - 1);
		expect(nearestIdxAtX(p.x + p.w / 2, n)).toBe(50);
		// A point that is less than half a bin away selects that bin. A point that is more than
		// half a bin away selects the nearer bin.
		expect(nearestIdxAtX(p.x + (p.w / (n - 1)) * 24.1, n)).toBe(24);
		expect(nearestIdxAtX(p.x + (p.w / (n - 1)) * 24.6, n)).toBe(25);
	});

	it('clamps outside the plot instead of returning a bogus index', () => {
		expect(nearestIdxAtX(p.x - 500, n)).toBe(0);
		expect(nearestIdxAtX(p.x + p.w + 500, n)).toBe(n - 1);
	});

	it('survives a degenerate array', () => {
		expect(nearestIdxAtX(p.x, 1)).toBe(0);
		expect(nearestIdxAtX(p.x, 0)).toBe(0);
	});
});

describe('top-left stack layout', () => {
	it('uses the corner line itself when nothing owns it (plain swept view)', () => {
		expect(topLeftLines()).toEqual({ readout: 0, osd: 1, shareMode: false });
	});

	it('moves the readout below the wide 3 dB BW label when it is shown', () => {
		// Only the presence of the measurement matters here, not its numbers.
		S.setM3dB({} as any);
		expect(topLeftLines()).toEqual({ readout: 1, osd: 2, shareMode: false });
	});

	it('shares the mode badge line in the real-time view', () => {
		S.setViewMode('rta');
		expect(topLeftLines()).toEqual({ readout: 0, osd: 1, shareMode: true });
	});

	it('keeps the readout off line 0 for the views that label it themselves', () => {
		S.setViewMode('harm');
		expect(topLeftLines()).toEqual({ readout: 1, osd: 2, shareMode: false });
		S.setViewMode('pnm');
		expect(topLeftLines()).toEqual({ readout: 1, osd: 2, shareMode: false });
	});

	it('ignores a stale 3 dB reading while the real-time view is active', () => {
		// render3dB runs on the swept path only, so its leftover state must not move the readout off
		// the badge line.
		S.setM3dB({} as any);
		S.setViewMode('rta');
		expect(topLeftLines()).toEqual({ readout: 0, osd: 1, shareMode: true });
	});

	it('never gives the readout and the marker lines the same line', () => {
		for (const mode of ['std', 'rta', 'sdr', 'harm', 'pnm']) {
			S.setViewMode(mode);
			const { readout, osd } = topLeftLines();
			expect(osd).toBeGreaterThan(readout);
			// Only the real-time view may share line 0 with the mode badge.
			expect(readout).toBeLessThanOrEqual(1);
		}
	});
});
