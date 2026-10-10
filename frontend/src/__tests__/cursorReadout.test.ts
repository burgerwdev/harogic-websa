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
import { nearestIdxAtX, plotRect } from '../render/plot';
import { topLeftLines } from '../render/spectrum';

beforeEach(() => {
	S.setViewMode('std');
	S.setM3dB(null);
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
