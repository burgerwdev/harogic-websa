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

// Reused by sparse projections; at most 2 * bins - 1 monotone quantile segments.
let sparseLeft = new Float64Array(0), sparseRight = new Float64Array(0);
let sparseFromLow = new Float64Array(0), sparseFromHigh = new Float64Array(0);
let sparseToLow = new Float64Array(0), sparseToHigh = new Float64Array(0);
let sparseMass = new Float64Array(0);

export interface DensityProjection {
	/** Destination amplitude rows; native row r maps to origin + r * scale. */
	bins: number;
	origin: number;
	scale: number;
}

/** Mean row coverage; ordered endpoints and their reciprocal span are prepared once. */
function clippedRampMean(start: number, end: number, inverseSpan: number): number {
	if (start >= 1) return 1;
	if (end <= 0) return 0;
	if (inverseSpan === 0) return Math.max(0, Math.min(1, (start + end) / 2));
	if (start >= 0 && end <= 1) return (start + end) / 2;
	const a = start <= 0 ? 0 : start * start / 2;
	const b = end >= 1 ? end - 0.5 : end * end / 2;
	return (b - a) * inverseSpan;
}

/**
 * Join adjacent occupied histograms by normalized cumulative mass. Each amplitude bin
 * has uniform support from row - 0.5 to row + 0.5; matched quantile intervals move as
 * continuous bands, so changing weights cannot switch whole delta lines on or off.
 * Native history stays untouched. Gaps retain finite native points without extrapolation.
 * Apart from clearing out, work is O(visible pairs * bins + pixels * occupied segments
 * + crossed amplitude cells), with O(bins) scratch and no per-pixel allocations or sorting.
 */
export function rasterSparseDensity(
	grid: DensityGrid, freq: Float64Array, lo: number, hi: number,
	width: number, out: Float32Array,
	projection?: DensityProjection,
): void {
	out.fill(0);
	if (!(hi > lo) || !(width > 0) || freq.length !== grid.heads.length) return;
	const bins = grid.bins;
	const outputBins = projection?.bins ?? bins;
	const origin = projection?.origin ?? 0, rowScale = projection?.scale ?? 1;
	if (sparseLeft.length < bins) {
		sparseLeft = new Float64Array(bins);
		sparseRight = new Float64Array(bins);
		sparseFromLow = new Float64Array(2 * bins);
		sparseFromHigh = new Float64Array(2 * bins);
		sparseToLow = new Float64Array(2 * bins);
		sparseToHigh = new Float64Array(2 * bins);
		sparseMass = new Float64Array(2 * bins);
	}
	const [first, end] = visibleBinRange(freq, lo, hi);
	const scale = width / (hi - lo);
	// Buffers persist across calls; clear once here, then each pair clears only its own rows.
	sparseLeft.fill(0, 0, bins);
	sparseRight.fill(0, 0, bins);
	for (let i = first; i + 1 < end; i++) {
		if (!Number.isFinite(grid.samples[i]) || !Number.isFinite(grid.samples[i + 1])) continue;
		const left = freq[i], right = freq[i + 1];
		if (!(right > left) || right < lo || left > hi) continue;
		// Clear through the sparse lists below; they already record exactly the touched rows.
		let leftTotal = 0, rightTotal = 0;
		for (let cell = grid.heads[i]; cell !== -1; cell = grid.next[cell]) {
			const row = cell % bins;
			sparseLeft[row] = grid.values[cell];
			leftTotal += grid.values[cell];
		}
		for (let cell = grid.heads[i + 1]; cell !== -1; cell = grid.next[cell]) {
			const row = cell % bins;
			sparseRight[row] = grid.values[cell];
			rightTotal += grid.values[cell];
		}
		if (!(leftTotal > 0) || !(rightTotal > 0)) {
			for (let cell = grid.heads[i]; cell !== -1; cell = grid.next[cell]) sparseLeft[cell % bins] = 0;
			for (let cell = grid.heads[i + 1]; cell !== -1; cell = grid.next[cell]) sparseRight[cell % bins] = 0;
			continue;
		}
		let a = 0, b = 0, aMass = 0, bMass = 0, aUsed = 0, bUsed = 0, segments = 0;
		while (true) {
			while (a < bins && !(aMass > 0)) { aMass = sparseLeft[a++] / leftTotal; aUsed = 0; }
			while (b < bins && !(bMass > 0)) { bMass = sparseRight[b++] / rightTotal; bUsed = 0; }
			if (!(aMass > 0) || !(bMass > 0)) break;
			const mass = Math.min(aMass, bMass);
			const aTotal = sparseLeft[a - 1] / leftTotal, bTotal = sparseRight[b - 1] / rightTotal;
			const fromLow = a - 1.5 + aUsed / aTotal;
			const fromHigh = a - 1.5 + (aUsed + mass) / aTotal;
			// Store projected origins and deltas once, outside the screen-pixel loop.
			sparseFromLow[segments] = origin + fromLow * rowScale;
			sparseFromHigh[segments] = origin + fromHigh * rowScale;
			sparseToLow[segments] = (b - 1.5 + bUsed / bTotal - fromLow) * rowScale;
			sparseToHigh[segments] = (b - 1.5 + (bUsed + mass) / bTotal - fromHigh) * rowScale;
			sparseMass[segments++] = mass;
			aMass -= mass; bMass -= mass;
			aUsed += mass; bUsed += mass;
		}
		// Half-open frequency intervals assign each pixel centre to exactly one pair;
		// the integrated band footprint spans steep slopes without skipping screen rows.
		const x0 = Math.max(0, Math.ceil((left - lo) * scale - 0.5));
		const x1 = Math.min(width, Math.ceil((right - lo) * scale - 0.5));
		for (let x = x0; x < x1; x++) {
			const t = (lo + (x + 0.5) / scale - left) / (right - left);
			// Keep the footprint centred at native endpoints as well as in the interior.
			const radius = Math.min(0.5 / (scale * (right - left)), t, 1 - t);
			const t0 = t - radius, t1 = t + radius;
			const total = leftTotal + (rightTotal - leftTotal) * t;
			for (let s = 0; s < segments; s++) {
				const fromLow = sparseFromLow[s], fromHigh = sparseFromHigh[s];
				const deltaLow = sparseToLow[s], deltaHigh = sparseToHigh[s];
				const low0 = fromLow + deltaLow * t0, low1 = fromLow + deltaLow * t1;
				const high0 = fromHigh + deltaHigh * t0, high1 = fromHigh + deltaHigh * t1;
				const area = ((high0 - low0) + (high1 - low1)) / 2;
				if (!(area > 0)) continue;
				const weight = total * sparseMass[s] / area;
				const lowMin = Math.min(low0, low1), lowMax = Math.max(low0, low1);
				const highMin = Math.min(high0, high1), highMax = Math.max(high0, high1);
				const lowSpan = lowMax - lowMin, highSpan = highMax - highMin;
				const inverseLow = lowSpan < 1e-9 ? 0 : 1 / lowSpan;
				const inverseHigh = highSpan < 1e-9 ? 0 : 1 / highSpan;
				const firstRow = Math.max(0, Math.floor(lowMin + 0.5));
				const endRow = Math.min(outputBins, Math.ceil(highMax + 0.5));
				for (let bin = firstRow; bin < endRow; bin++) {
					const edge = bin - 0.5;
					const coverage = highMin >= edge + 1 && lowMax <= edge ? 1
						: clippedRampMean(highMin - edge, highMax - edge, inverseHigh)
							- clippedRampMean(lowMin - edge, lowMax - edge, inverseLow);
					out[bin * width + x] += weight * Math.max(0, coverage);
				}
			}
		}
		for (let cell = grid.heads[i]; cell !== -1; cell = grid.next[cell]) sparseLeft[cell % bins] = 0;
		for (let cell = grid.heads[i + 1]; cell !== -1; cell = grid.next[cell]) sparseRight[cell % bins] = 0;
	}
	// Resolved pairs already contain their endpoints as continuous bands. Only isolated
	// points, an uncovered right endpoint, and subpixel carriers need a native projection.
	for (let i = first; i < end; i++) {
		if (!Number.isFinite(grid.samples[i]) || freq[i] < lo || freq[i] > hi || grid.heads[i] === -1) continue;
		const x = Math.min(width - 1, Math.floor((freq[i] - lo) * scale));
		const hasLeft = i > 0 && Number.isFinite(grid.samples[i - 1]) && grid.heads[i - 1] !== -1 && freq[i] > freq[i - 1];
		const hasRight = i + 1 < freq.length && Number.isFinite(grid.samples[i + 1]) && grid.heads[i + 1] !== -1 && freq[i + 1] > freq[i];
		// A one-pixel span can put both endpoints in the same clamped device column.
		// It cannot resolve their separate peaks, so retain the native MAX there too.
		const subpixel = (hasLeft && (freq[i] - freq[i - 1]) * scale <= 1)
			|| (hasRight && (freq[i + 1] - freq[i]) * scale <= 1);
		const centreFreq = lo + (x + 0.5) / scale;
		const covered = (hasLeft && centreFreq >= freq[i - 1] && centreFreq < freq[i])
			|| (hasRight && centreFreq >= freq[i] && centreFreq < freq[i + 1]);
		if (!subpixel && (hasLeft || hasRight) && covered) continue;
		sparseLeft.fill(0, 0, bins);
		let firstBin = bins, endBin = 0;
		for (let cell = grid.heads[i]; cell !== -1; cell = grid.next[cell]) {
			const bin = cell % bins;
			sparseLeft[bin] = grid.values[cell];
			firstBin = Math.min(firstBin, bin); endBin = Math.max(endBin, bin + 1);
		}
		const firstRow = Math.max(0, Math.floor(origin + (firstBin - 0.5) * rowScale + 0.5));
		const endRow = Math.min(outputBins, Math.ceil(origin + (endBin - 0.5) * rowScale + 0.5));
		for (let row = firstRow; row < endRow; row++) {
			const low = (row - 0.5 - origin) / rowScale, high = (row + 0.5 - origin) / rowScale;
			let value = 0;
			for (let bin = Math.max(firstBin, Math.floor(low + 0.5)); bin < Math.min(endBin, Math.ceil(high + 0.5)); bin++) {
				value += sparseLeft[bin] * Math.max(0, Math.min(high, bin + 0.5) - Math.max(low, bin - 0.5));
			}
			const k = row * width + x;
			out[k] = Math.max(out[k], value);
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

export interface DensityPalette {
	colours: Uint32Array;
	alpha: Float32Array;
	maxIndex: number;
	rasterBits?: Uint32Array;
}

/** Float32 exponent/mantissa buckets keep relative precision down to faint afterglow. */
export function densityPalette(contrast: number, lut: Uint32Array): DensityPalette {
	const colours = new Uint32Array(65536), alpha = new Float32Array(65536);
	const sample = new Float32Array(1), bits = new Uint32Array(sample.buffer);
	sample[0] = MAX_DENSITY;
	const maxIndex = bits[0] >>> 15;
	for (let i = 0; i < colours.length; i++) {
		// Midpoint of each bucket; above the density cap every entry saturates.
		bits[0] = (Math.min(i, maxIndex) << 15) | 0x4000;
		const n = Math.min(1, sample[0] / MAX_DENSITY);
		colours[i] = lut[Math.round(255 * Math.pow(n, 0.45 * contrast))];
		alpha[i] = 255 * (1 - Math.exp(-5 * Math.pow(n, 0.4)));
	}
	return { colours, alpha, maxIndex };
}

let sparsePaintCoverage = new Float64Array(0);

/**
 * Paint native amplitude centres into a reusable screen-resolution RGBA image.
 * Width is a centred display footprint in backing pixels, never an amplitude spread.
 * Max opacity composites overlaps without adding occupancy; fractional edge coverage
 * keeps the centre and CSS width stable across pixel ratios and amplitude gestures.
 */
export function paintDensity(
	raster: Float32Array, cols: number, bins: number, pixels: Uint8ClampedArray,
	rows: number, firstCentre: number, rowPitch: number, footprint: number,
	contrast: number, lut: Uint32Array, softFootprint = false, palette?: DensityPalette,
): void {
	pixels.fill(0);
	const half = footprint / 2;
	const feather = Math.min(1, half);
	const preparedCoverage = softFootprint && !!palette;
	if (preparedCoverage && sparsePaintCoverage.length < rows) sparsePaintCoverage = new Float64Array(rows);
	if (palette && (!palette.rasterBits || palette.rasterBits.buffer !== raster.buffer
		|| palette.rasterBits.byteOffset !== raster.byteOffset || palette.rasterBits.length !== raster.length)) {
		palette.rasterBits = new Uint32Array(raster.buffer, raster.byteOffset, raster.length);
	}
	for (let bin = 0; bin < bins; bin++) {
		const centre = firstCentre + bin * rowPitch;
		const top = centre - half, bottom = centre + half;
		const y0 = Math.max(0, Math.floor(top)), y1 = Math.min(rows, Math.ceil(bottom));
		if (y0 >= y1) continue;
		if (preparedCoverage) {
			// Coverage depends on the footprint and output row, never the column's mass.
			for (let y = y0; y < y1; y++) {
				const start = Math.max(y, top) - top, end = Math.min(y + 1, bottom) - top;
				let coverage = end - start;
				if (start < feather) {
					const a = feather - start, b = Math.max(0, feather - end);
					coverage -= (a * a - b * b) / (2 * feather);
				}
				if (end > footprint - feather) {
					const a = end - footprint + feather, b = Math.max(0, start - footprint + feather);
					coverage -= (a * a - b * b) / (2 * feather);
				}
				sparsePaintCoverage[y] = coverage;
			}
		}
		for (let x = 0; x < cols; x++) {
			const value = raster[bin * cols + x];
			if (!(value > 0)) continue;
			let colour: number, alpha: number;
			if (palette) {
				const index = Math.min(palette.maxIndex, palette.rasterBits![bin * cols + x] >>> 15);
				colour = palette.colours[index];
				alpha = palette.alpha[index];
			} else {
				const n = Math.min(1, value / MAX_DENSITY);
				colour = lut[Math.round(255 * Math.pow(n, 0.45 * contrast))];
				// Visibility depends only on occupancy, independently of the colour curve.
				alpha = 255 * (1 - Math.exp(-5 * Math.pow(n, 0.4)));
			}
			for (let y = y0; y < y1; y++) {
				let coverage: number;
				if (preparedCoverage) coverage = sparsePaintCoverage[y];
				else {
					const start = Math.max(y, top) - top, end = Math.min(y + 1, bottom) - top;
					coverage = end - start;
					if (softFootprint) {
						// Feather only the outer backing pixel inside the original footprint.
						if (start < feather) {
							const a = feather - start, b = Math.max(0, feather - end);
							coverage -= (a * a - b * b) / (2 * feather);
						}
						if (end > footprint - feather) {
							const a = end - footprint + feather, b = Math.max(0, start - footprint + feather);
							coverage -= (a * a - b * b) / (2 * feather);
						}
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
