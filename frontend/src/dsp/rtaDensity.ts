/**
 * The 2D probability-density grid behind the RTA / SDR density display.
 *
 * Extracted from core/ws.ts: the rules took real debugging (the weight, the saturation, the grid
 * anchor) and they are worth testing without a canvas, a socket or a frame payload. It also puts
 * the persistence choice in one place - with persistence OFF the grid is neither advanced nor
 * kept, which is what makes "Off" mean the whole layer is off, and skips the bins x points loop
 * this display otherwise pays for on every frame.
 *
 * The grid is a Float32Array of `points * bins`: row-major by frequency, bins ordered downward in
 * level (bin 0 = the display reference line), which is the order the renderer draws it in.
 */
import { percentileApprox } from './stats';

/** Density that saturates: past it the colour map cannot get any hotter. Also read by the renderer. */
export const MAX_DENSITY = 40;
/** Accumulated weight below this is dropped to zero, so a fading grid really does reach zero. */
const DENSITY_FLOOR = 0.05;

/**
 * Advance the grid by one frame.
 *
 * @param prev    the grid on screen; null, or a grid from another (points, bins) geometry, starts
 *                a fresh one
 * @param spec    the frame's amplitudes in dBm
 * @param bins    amplitude bins per column
 * @param fade    per-frame decay; <= 0 means persistence is OFF
 * @param refTop  level of row 0 (the display reference of the frame being accumulated)
 * @param dBPerBin level height of one bin
 * @returns the grid to draw, or null when persistence is off (the caller releases it)
 */
export function advanceDensity(
	prev: Float32Array | null,
	spec: Float32Array,
	bins: number,
	fade: number,
	refTop: number,
	dBPerBin: number,
): Float32Array | null {
	if (!(fade > 0)) return null;
	if (!(bins > 0) || !(dBPerBin > 0)) return prev;
	const len = spec.length * bins;
	const reuse = prev !== null && prev.length === len;
	const grid = reuse ? prev : new Float32Array(len);
	// A fresh grid starts from this frame alone: decaying an empty array would be a no-op, but the
	// distinction matters for the first frame after a geometry change (it must not be halved).
	const decay = reuse ? fade : 0;
	const floorN = percentileApprox(spec, 0.3);
	for (let i = 0; i < spec.length; i++) {
		if (decay > 0) {
			for (let b = 0; b < bins; b++) {
				const v = grid[i * bins + b] * decay;
				grid[i * bins + b] = v > DENSITY_FLOOR ? v : 0;
			}
		}
		// Amplitude-graded weight: how far a point sits above the noise floor decides how strongly
		// it accumulates. Weak signals (>3 dB) still leave a light density cloud so the map covers
		// the whole trace; the floor ripple itself stays out.
		const relDb = spec[i] - floorN;
		const w = relDb < 3 ? 0 : relDb >= 25 ? 1 : 0.25 + 0.75 * ((relDb - 3) / 22);
		if (w <= 0) continue;
		const bin = Math.max(0, Math.min(bins - 1, Math.round((refTop - spec[i]) / dBPerBin)));
		const base = i * bins;
		// A five-bin kernel (2-2-3.5-2-2 weights) widens the hit so the drawn layer is continuous
		// instead of a one-pixel line; the peak bin scales with the signal strength.
		bump(grid, base, bin, bins, 3.5 * w);
		bump(grid, base, bin - 1, bins, 2 * w);
		bump(grid, base, bin + 1, bins, 2 * w);
		bump(grid, base, bin - 2, bins, 1 * w);
		bump(grid, base, bin + 2, bins, 1 * w);
	}
	return grid;
}

function bump(grid: Float32Array, base: number, bin: number, bins: number, weight: number): void {
	if (bin < 0 || bin >= bins) return;
	const k = base + bin;
	const v = grid[k] + weight;
	grid[k] = v > MAX_DENSITY ? MAX_DENSITY : v;
}
