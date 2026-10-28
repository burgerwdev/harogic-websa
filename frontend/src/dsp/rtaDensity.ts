/** Native-frequency persistence. Only occupied amplitude cells are visited for decay. */
import { percentileApprox } from './stats';
import { sameFrequencyGrid, visibleBinRange } from './grid';

export const MAX_DENSITY = 40;
const DENSITY_FLOOR = 0.05;
/** Hit weight per second at the 30 fps reference; decay is real-time, so hits follow it. */
export const DENSITY_HIT_RATE = 105;

export type DensitySource = 'trace' | 'live';

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
	/** Which stream the history speaks for; switching it starts a new history. */
	source: DensitySource;
	/** Wall clock of the last advance; -1 means the next advance uses the reference cadence. */
	lastMs: number;
}

/**
 * The input is the trace-following density's active trace AFTER its mode accumulator, or the
 * raw received frame for the live source. Geometry changes start a new history; while paused,
 * the old geometry remains available for display reprojection. Persistence Off releases
 * history. VIEW retains it without decay or repeated frozen hits.
 * Decay is per real second (half-life semantics); dt is clamped so a tab wake neither floods
 * nor wipes the history, and no occupancy is invented for the silent interval.
 */
export function advanceDensity(
	prev: DensityGrid | null,
	spec: Float32Array | null,
	bins: number,
	halflifeSec: number,
	refTop: number,
	dBPerBin: number,
	mode = 'CLEAR_WRITE',
	freq?: Float64Array,
	source: DensitySource = 'trace',
	nowMs?: number,
): DensityGrid | null {
	if (!(halflifeSec > 0) || mode === 'OFF' || !spec) return null;
	const sameAxis = !freq || (prev?.freq !== undefined && sameFrequencyGrid(prev.freq, freq));
	if (mode === 'VIEW' && source === 'trace' && prev && prev.source === source && sameAxis) { prev.freq = freq; return prev; }
	if (!(bins > 0) || !(dBPerBin > 0)) return prev;
	const reuse = prev !== null && prev.heads.length === spec.length && prev.bins === bins
		&& prev.refTop === refTop && prev.dBPerBin === dBPerBin && sameAxis && prev.source === source;
	const grid: DensityGrid = reuse ? prev! : {
		values: new Float32Array(spec.length * bins),
		next: new Int32Array(spec.length * bins),
		heads: new Int32Array(spec.length).fill(-1),
		bins, refTop, dBPerBin, revision: 0, samples: spec, source, lastMs: -1,
	};
	grid.freq = freq;
	grid.samples = spec;
	const now = nowMs ?? performance.now();
	// First advance has no elapsed time to decay over; one frame of hits enters at the
	// reference cadence. A burst of backlog frames decays by its wall-clock span, not its
	// frame count. The 2 s clamp bounds a tab wake: no flood-in, no wipe, and no occupancy
	// invented for the silent interval.
	const dt = reuse && grid.lastMs >= 0 ? Math.min(Math.max((now - grid.lastMs) / 1000, 0), 2) : 1 / 30;
	grid.lastMs = now;
	const decay = Math.pow(2, -dt / halflifeSec);
	const hitWeight = DENSITY_HIT_RATE * Math.min(dt, 0.25);
	const floorN = percentileApprox(spec, 0.3);
	for (let i = 0; i < spec.length; i++) {
		let previous = -1;
		let cell = grid.heads[i];
		while (cell !== -1) {
			const next = grid.next[cell];
			const v = grid.values[cell] * decay;
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
		// Keep measured noise visible without a discontinuity at the old 3 dB cutoff.
		// This changes hit strength only: each hit still occupies its measured amplitude.
		const w = 0.12 + 0.88 * Math.min(1, Math.max(0, relDb) / 25);
		// Do not clamp off-screen samples to an edge: that invents a horizontal signal.
		const row = (refTop - spec[i]) / dBPerBin;
		const bin = Math.floor(row);
		const fraction = row - bin;
		// Fractional hits preserve the trace's level instead of snapping five full rows on.
		bump(grid, i, bin, hitWeight * w * (1 - fraction));
		bump(grid, i, bin + 1, hitWeight * w * fraction);
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
			if ((!resolved || left === right) && hz >= lo && hz <= hi) out[base + peakX] = Math.max(out[base + peakX], value);
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

/** Theme colours stay distinct from the plot background even for rare observations. */
export function densityLut(light: boolean): Uint32Array {
	const stops = light
		? [[35, 55, 165], [25, 100, 190], [0, 130, 150], [70, 135, 60], [170, 125, 0], [210, 65, 0]]
		: [[35, 95, 220], [20, 150, 235], [25, 205, 205], [105, 230, 90], [255, 220, 40], [255, 100, 20]];
	const lut = new Uint32Array(256);
	for (let i = 0; i < lut.length; i++) {
		const x = i / 255 * (stops.length - 1);
		const j = Math.min(stops.length - 2, Math.floor(x));
		const t = x - j, a = stops[j], b = stops[j + 1];
		const r = Math.round(a[0] + (b[0] - a[0]) * t);
		const g = Math.round(a[1] + (b[1] - a[1]) * t);
		const blue = Math.round(a[2] + (b[2] - a[2]) * t);
		lut[i] = (blue << 16) | (g << 8) | r;
	}
	return lut;
}

/**
 * Paint native amplitude centres into a reusable screen-resolution RGBA image.
 * Width is a centred display footprint in backing pixels, never an amplitude spread.
 * Max opacity composites overlaps without adding occupancy; fractional edge coverage
 * keeps the centre and CSS width stable across pixel ratios and amplitude gestures.
 */
export function paintDensity(
	raster: Float32Array, cols: number, bins: number, pixels: Uint8ClampedArray,
	rows: number, firstCentre: number, rowPitch: number, footprint: number,
	contrast: number, lut: Uint32Array, softFootprint = false,
): void {
	pixels.fill(0);
	const half = footprint / 2;
	const feather = Math.min(1, half);
	for (let bin = 0; bin < bins; bin++) {
		const centre = firstCentre + bin * rowPitch;
		const top = centre - half, bottom = centre + half;
		const y0 = Math.max(0, Math.floor(top)), y1 = Math.min(rows, Math.ceil(bottom));
		if (y0 >= y1) continue;
		for (let x = 0; x < cols; x++) {
			const value = raster[bin * cols + x];
			if (!(value > 0)) continue;
			const n = Math.min(1, value / MAX_DENSITY);
			const colour = lut[Math.round(255 * Math.pow(n, 0.45 * contrast))];
			// Visibility depends only on occupancy, independently of the colour curve.
			const alpha = 255 * (1 - Math.exp(-5 * Math.pow(n, 0.4)));
			for (let y = y0; y < y1; y++) {
				const start = Math.max(y, top) - top, end = Math.min(y + 1, bottom) - top;
				let coverage = end - start;
				if (softFootprint) {
					// Keep a flat centre; feather only the outer backing pixel inside the
					// original footprint so separated amplitude rows do not become thin stripes.
					if (start < feather) {
						const a = feather - start, b = Math.max(0, feather - end);
						coverage -= (a * a - b * b) / (2 * feather);
					}
					if (end > footprint - feather) {
						const a = end - footprint + feather, b = Math.max(0, start - footprint + feather);
						coverage -= (a * a - b * b) / (2 * feather);
					}
				}
				const a = Math.round(alpha * coverage), o = (y * cols + x) * 4;
				if (a <= pixels[o + 3]) continue;
				pixels[o] = colour & 0xff;
				pixels[o + 1] = (colour >> 8) & 0xff;
				pixels[o + 2] = (colour >> 16) & 0xff;
				pixels[o + 3] = a;
			}
		}
	}
}
