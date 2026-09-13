/**
 * SDR auto-reference exponential moving averages (leaf).
 *
 * They are written by the SDR auto-scale block in core/ws.ts and reset by
 * ui/controls.ts (entering SDR, preset). Keeping the state here lets controls reset it
 * without importing core/ws.ts, which used to be a two-module cycle
 * (docs/.../ARCH_REVIEW.md finding P1-4).
 */

export const sdrAutoRef = {
	noiseEma: -120,
	peakEma: -120,      // must satisfy the < -119 seed guard in the auto-ref block
	lastAt: 0,
};

export function resetSdrAutoRef(): void {
	sdrAutoRef.noiseEma = -120;
	sdrAutoRef.peakEma = -120;
	sdrAutoRef.lastAt = 0;
}
