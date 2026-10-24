/** Native-frequency persistence. Only occupied amplitude cells are visited for decay. */
import { percentileApprox } from './stats';
import { sameFrequencyGrid, visibleBinRange } from './grid';

export const MAX_DENSITY = 40;
const DENSITY_FLOOR = 0.05;

export interface DensityGrid {
	values: Float32Array;
	/** Linked occupied cells per native frequency column; -1 terminates each list. */
	next: Int32Array;
	heads: Int32Array;
	bins: number;
	/** Raw dBm at amplitude row zero; display offset is applied only when drawing. */
	refTop: number;
	dBPerBin: number;
	revision: number;
	/** The native axis that produced this history; the receiver updates it with its frame. */
	freq?: Float64Array;
	samples: Float32Array;
}

/**
 * The input is the active trace AFTER its mode accumulator. Geometry changes start a new
 * history; while paused, the old geometry remains available for display reprojection.
 * Persistence Off releases history. VIEW retains it without decay or repeated frozen hits.
 */
export function advanceDensity(
	prev: DensityGrid | null,
	spec: Float32Array | null,
	bins: number,
	fade: number,
	refTop: number,
	dBPerBin: number,
	mode = 'CLEAR_WRITE',
	freq?: Float64Array,
): DensityGrid | null {
	if (!(fade > 0) || mode === 'OFF' || !spec) return null;
	const sameAxis = !freq || (prev?.freq !== undefined && sameFrequencyGrid(prev.freq, freq));
	if (mode === 'VIEW' && prev && sameAxis) { prev.freq = freq; return prev; }
	if (!(bins > 0) || !(dBPerBin > 0)) return prev;
	const reuse = prev !== null && prev.heads.length === spec.length && prev.bins === bins
		&& prev.refTop === refTop && prev.dBPerBin === dBPerBin && sameAxis;
	const grid: DensityGrid = reuse ? prev! : {
		values: new Float32Array(spec.length * bins),
		next: new Int32Array(spec.length * bins),
		heads: new Int32Array(spec.length).fill(-1),
		bins, refTop, dBPerBin, revision: 0, samples: spec,
	};
	grid.freq = freq;
	grid.samples = spec;
	const floorN = percentileApprox(spec, 0.3);
	for (let i = 0; i < spec.length; i++) {
		let previous = -1;
		let cell = grid.heads[i];
		while (cell !== -1) {
			const next = grid.next[cell];
			const v = grid.values[cell] * fade;
			if (v > DENSITY_FLOOR) {
				grid.values[cell] = v;
				previous = cell;
			} else {
				grid.values[cell] = 0;
				if (previous === -1) grid.heads[i] = next;
				else grid.next[previous] = next;
			}
			cell = next;
		}
		if (!Number.isFinite(spec[i])) continue;
		const relDb = spec[i] - floorN;
		const w = relDb < 3 ? 0 : relDb >= 25 ? 1 : 0.25 + 0.75 * ((relDb - 3) / 22);
		if (w <= 0) continue;
		// Do not clamp off-screen samples to an edge: that invents a horizontal signal.
		const row = (refTop - spec[i]) / dBPerBin;
		const bin = Math.floor(row);
		const fraction = row - bin;
		// Fractional hits preserve the trace's level instead of snapping five full rows on.
		bump(grid, i, bin, 3.5 * w * (1 - fraction));
		bump(grid, i, bin + 1, 3.5 * w * fraction);
	}
	grid.revision++;
	return grid;
}

function bump(grid: DensityGrid, column: number, bin: number, weight: number): void {
	if (bin < 0 || bin >= grid.bins || !(weight > 0)) return;
	const k = column * grid.bins + bin;
	if (grid.values[k] === 0) {
		grid.next[k] = grid.heads[column];
		grid.heads[column] = k;
	}
	grid.values[k] = Math.min(MAX_DENSITY, grid.values[k] + weight);
}

/**
 * Max-composite linearly reconstructed native frequency samples into a reusable raster.
 * Wide native bins taper to their neighbours instead of expanding into rectangular cells.
 * Subpixel bins retain their peak in a device column so narrow carriers survive zoom-out.
 */
export function rasterDensity(
	grid: DensityGrid, freq: Float64Array, lo: number, hi: number,
	width: number, out: Float32Array,
): void {
	out.fill(0);
	if (!(hi > lo) || freq.length !== grid.heads.length) return;
	const [first, end] = visibleBinRange(freq, lo, hi);
	const scale = width / (hi - lo);
	for (let i = first; i < end; i++) {
		if (!Number.isFinite(grid.samples[i])) continue;
		const hz = freq[i];
		const left = i === 0 || !Number.isFinite(grid.samples[i - 1]) ? hz : freq[i - 1];
		const right = i === freq.length - 1 || !Number.isFinite(grid.samples[i + 1]) ? hz : freq[i + 1];
		if (right < lo || left > hi) continue;
		const x0 = Math.max(0, Math.floor((left - lo) * scale));
		const x1 = Math.min(width, Math.ceil((right - lo) * scale));
		const peakX = Math.min(width - 1, Math.floor((hz - lo) * scale));
		const resolved = (i === 0 || (hz - freq[i - 1]) * scale >= 1)
			&& (i === freq.length - 1 || (freq[i + 1] - hz) * scale >= 1);
		for (let cell = grid.heads[i]; cell !== -1; cell = grid.next[cell]) {
			const base = (cell % grid.bins) * width;
			const value = grid.values[cell];
			for (let x = x0; x < x1; x++) {
				const sampleHz = lo + (x + 0.5) / scale;
				if (sampleHz < left || sampleHz > right) continue;
				const weight = sampleHz <= hz
					? (hz === left ? 1 : (sampleHz - left) / (hz - left))
					: (hz === right ? 1 : (right - sampleHz) / (right - hz));
				const contribution = value * Math.max(0, Math.min(1, weight));
				out[base + x] = resolved ? out[base + x] + contribution : Math.max(out[base + x], contribution);
			}
			if (!resolved && hz >= lo && hz <= hi) out[base + peakX] = Math.max(out[base + peakX], value);
		}
	}
}

/** Destination geometry: a raster row's CENTRE is its raw-dBm bin, just like the trace. */
export function densityLevelPlacement(
	grid: DensityGrid, ref: number, range: number, offset: number,
): { top: number; height: number } {
	return {
		top: (ref - grid.refTop - offset - grid.dBPerBin / 2) / range,
		height: grid.bins * grid.dBPerBin / range,
	};
}
