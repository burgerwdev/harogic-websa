/** Native-frequency persistence. Only occupied amplitude cells are visited for decay. */
import { percentileApprox } from './stats';
import { sameFrequencyGrid, visibleBinRange } from './grid';

export const MAX_DENSITY = 40;
const DENSITY_FLOOR = 0.05;
/** Hit weight per second at the 30 fps reference; decay is real-time, so hits follow it. */
export const DENSITY_HIT_RATE = 105;

export type DensitySource = 'trace' | 'live';

/** SDR stores measured contours, never the native-frequency × amplitude cell grid. */
export const SDR_DENSITY_HISTORY_BYTES = 8 * 1024 * 1024;
export const SDR_DENSITY_HISTORY_FRAMES = 128;
export interface DensityContour {
	samples: Float32Array;
	weight: number;
	noise: number;
}
export interface DensityHistory {
	frames: DensityContour[];
	capacity: number;
	/** Deterministic weighted reservoir sampling when adjacent time buckets merge. */
	seed: number;
}

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
	/** Present only for SDR; the legacy cell arrays are empty in this representation. */
	history?: DensityHistory;
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
	sdr = false,
): DensityGrid | null {
	if (!(halflifeSec > 0) || mode === 'OFF' || !spec) return null;
	if (sdr) return advanceContourDensity(prev, spec, bins, halflifeSec, refTop, dBPerBin,
		mode, freq, source, nowMs);
	const sameAxis = !freq || (prev?.freq !== undefined && sameFrequencyGrid(prev.freq, freq));
	if (mode === 'VIEW' && source === 'trace' && prev && !prev.history && sameAxis) { prev.freq = freq; return prev; }
	if (!(bins > 0) || !(dBPerBin > 0)) return prev;
	const reuse = prev !== null && !prev.history && prev.heads.length === spec.length && prev.bins === bins
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
 * A bounded time sample of actual received contours. Compaction merges neighbouring time
 * buckets by selecting one of their measured spectra proportional to its decayed mass;
 * amplitudes are never averaged. The selected contour carries both buckets' mass, so even
 * long half-lives retain their elapsed-time decay instead of cutting off after 128 packets.
 * Old shape distributions become a temporal sample, rather than invented intermediate peaks.
 * Retained sample payload is <=max(8 MiB, one native spectrum) and <=128 frames.
 * Very large captures retain one measured contour instead of allocating a cell grid.
 */
function advanceContourDensity(
	prev: DensityGrid | null, spec: Float32Array, bins: number, halflifeSec: number,
	refTop: number, dBPerBin: number, mode: string, freq: Float64Array | undefined,
	source: DensitySource, nowMs: number | undefined,
): DensityGrid | null {
	if (!spec.length) return null;
	const sameAxis = !freq || (prev?.freq !== undefined && sameFrequencyGrid(prev.freq, freq));
	const reuse = !!prev?.history && prev.samples.length === spec.length && sameAxis && prev.source === source;
	if (reuse && mode === 'VIEW' && source === 'trace') { prev!.freq = freq; return prev; }
	if (!(bins > 0) || !(dBPerBin > 0)) return prev;
	const grid: DensityGrid = reuse ? prev! : {
		values: new Float32Array(0), next: new Int32Array(0), heads: new Int32Array(0),
		bins, refTop, dBPerBin, revision: 0, samples: spec, source, lastMs: -1,
		history: { frames: [], capacity: Math.max(1, Math.min(SDR_DENSITY_HISTORY_FRAMES,
			Math.floor(SDR_DENSITY_HISTORY_BYTES / spec.byteLength))), seed: 0x51d7348b },
	};
	grid.freq = freq;
	const history = grid.history!;
	const now = nowMs ?? performance.now();
	const dt = reuse && grid.lastMs >= 0 ? Math.min(Math.max((now - grid.lastMs) / 1000, 0), 2) : 1 / 30;
	grid.lastMs = now;
	const decay = Math.pow(2, -dt / halflifeSec);
	let spare: Float32Array | undefined;
	for (let i = history.frames.length - 1; i >= 0; i--) {
		const frame = history.frames[i];
		frame.weight *= decay;
		if (frame.weight <= DENSITY_FLOOR) {
			spare = frame.samples;
			history.frames.splice(i, 1);
		}
	}
	const weight = DENSITY_HIT_RATE * Math.min(dt, 0.25);
	if (weight > 0) {
		const noise = percentileApprox(spec, 0.3);
		if (history.capacity === 1 && history.frames.length) {
			const frame = history.frames[0];
			if (contourChoice(history) < weight / (frame.weight + weight)) {
				frame.samples.set(spec); frame.noise = noise;
			}
			frame.weight += weight;
		} else {
			if (history.frames.length === history.capacity) {
				// Merge the least occupied adjacent time buckets, retaining recent detail.
				let pair = 0, mass = Infinity;
				for (let i = 0; i + 1 < history.frames.length; i++) {
					const sum = history.frames[i].weight + history.frames[i + 1].weight;
					if (sum < mass) { mass = sum; pair = i; }
				}
				const a = history.frames[pair], b = history.frames[pair + 1];
				const keepB = contourChoice(history) < b.weight / mass;
				const keep = keepB ? b : a;
				spare = keepB ? a.samples : b.samples;
				keep.weight = mass;
				history.frames.splice(pair, 2, keep);
			}
			const samples = spare ?? new Float32Array(spec.length);
			samples.set(spec);
			history.frames.push({ samples, weight, noise });
		}
	}
	grid.samples = history.frames[history.frames.length - 1]?.samples ?? spec;
	grid.revision++;
	return grid;
}

function contourChoice(history: DensityHistory): number {
	history.seed = (Math.imul(history.seed, 1664525) + 1013904223) >>> 0;
	return history.seed / 4294967296;
}

/**
 * Reconstruct the visible measured polyline in device pixels. Each contour integrates its
 * disjoint horizontal segment areas, then contributes its elapsed-time mass to the screen.
 * A single reusable column keeps scratch O(height), with no per-frame screen clear.
 * Dense columns retain first/min/max/last in native order, including NaN segment breaks.
 * Rendering samples at most 16 temporal buckets and 32768 visible native vertices (one
 * contour minimum). Each bucket uses its weighted median measured contour with the entire
 * bucket mass, making selection deterministic and keeping identical occupancy stable on zoom.
 * Stored history remains intact so zoom can recover the finer temporal distribution.
 */
export function rasterDensityContours(
	grid: DensityGrid, freq: Float64Array, lo: number, hi: number,
	cols: number, rows: number, firstCentre: number, rowPitch: number, footprint: number,
	out: Float32Array, column: Float32Array,
): void {
	out.fill(0);
	if (!(hi > lo) || !grid.history || freq.length !== grid.samples.length) return;
	const [first, end] = visibleBinRange(freq, lo, hi);
	const scale = cols / (hi - lo), half = footprint / 2;
	const frames = grid.history.frames;
	const buckets = Math.min(frames.length, 16, Math.max(1, Math.floor(32768 / Math.max(1, end - first))));
	for (let bucket = 0; bucket < buckets; bucket++) {
		const start = Math.floor(bucket * frames.length / buckets);
		const stop = Math.floor((bucket + 1) * frames.length / buckets);
		let weight = 0;
		for (let i = start; i < stop; i++) weight += frames[i].weight;
		let selected = start, cumulative = frames[start].weight;
		while (selected + 1 < stop && cumulative < weight / 2) cumulative += frames[++selected].weight;
		const frame = frames[selected], samples = frame.samples;
		let activeX = -1, low = rows, high = 0;
		let pen = -1;
		const flushColumn = () => {
			if (activeX < 0) return;
			for (let y = low; y < high; y++) {
				const k = y * cols + activeX;
				out[k] = Math.min(MAX_DENSITY, out[k] + column[y] * weight);
				column[y] = 0;
			}
			low = rows; high = 0;
		};
		const strength = (level: number) => 0.12 + 0.88 * Math.min(1, Math.max(0, level - frame.noise) / 25);
		const emit = (index: number) => {
			// Defer a run's first vertex to its outgoing segment. Only a truly isolated
			// vertex needs a stamp; otherwise the stamp would double-count that segment.
			if (pen < 0 && index + 1 < samples.length && Number.isFinite(samples[index + 1])) {
				pen = index;
				return;
			}
			const bx = (freq[index] - lo) * scale;
			const by = firstCentre + (grid.refTop - samples[index]) / grid.dBPerBin * rowPitch;
			const bw = strength(samples[index]);
			const ax = pen < 0 ? bx : (freq[pen] - lo) * scale;
			const ay = pen < 0 ? by : firstCentre + (grid.refTop - samples[pen]) / grid.dBPerBin * rowPitch;
			const aw = pen < 0 ? bw : strength(samples[pen]);
			// A standalone measured vertex gets a one-pixel horizontal footprint.
			const left = pen < 0 ? Math.floor(bx) : ax;
			const right = pen < 0 ? Math.floor(bx) + 1 : bx;
			pen = index;
			if (right < 0 || left >= cols || right <= left) return;
			for (let x = Math.max(0, Math.floor(left)); x < Math.min(cols, Math.ceil(right)); x++) {
				if (x !== activeX) { flushColumn(); activeX = x; }
				const l = Math.max(left, x), r = Math.min(right, x + 1);
				const t0 = (l - left) / (right - left), t1 = (r - left) / (right - left);
				const y0 = ay + (by - ay) * t0, y1 = ay + (by - ay) * t1;
				const weight = aw + (bw - aw) * (t0 + t1) / 2;
				const bottom = Math.min(rows, Math.ceil(Math.max(y0, y1) + half));
				const top = Math.max(0, Math.floor(Math.min(y0, y1) - half));
				low = Math.min(low, top); high = Math.max(high, bottom);
				for (let y = top; y < bottom; y++) {
					// Exact pixel area of the sloping vertical-width strip, not a horizontal bin.
					const coverage = (r - l) * (clampedRampMean(y0 + half - y, y1 + half - y)
						- clampedRampMean(y0 - half - y, y1 - half - y));
					// Adjacent native segments cover disjoint horizontal subintervals of this
					// pixel. Sum their areas so native boundaries cannot dim a flat contour.
					column[y] = Math.min(1, column[y] + coverage * weight);
				}
			}
		};
		let groupFirst = -1, groupLast = -1, min = -1, max = -1, groupX = -Infinity;
		const flushGroup = () => {
			if (groupFirst < 0) return;
			emit(groupFirst);
			const a = Math.min(min, max), b = Math.max(min, max);
			if (a > groupFirst && a < groupLast) emit(a);
			if (b > a && b > groupFirst && b < groupLast) emit(b);
			if (groupLast > groupFirst) emit(groupLast);
			groupFirst = -1;
		};
		for (let i = first; i < end; i++) {
			if (!Number.isFinite(samples[i])) { flushGroup(); pen = -1; continue; }
			const x = Math.floor((freq[i] - lo) * scale);
			if (groupFirst >= 0 && x !== groupX) flushGroup();
			if (groupFirst < 0) { groupFirst = min = max = i; groupX = x; }
			groupLast = i;
			if (samples[i] < samples[min]) min = i;
			if (samples[i] > samples[max]) max = i;
		}
		flushGroup(); flushColumn();
	}
}

/** Mean of clamp(t, 0, 1) along a linear segment, including horizontal segments. */
function clampedRampMean(a: number, b: number): number {
	if (Math.abs(b - a) < 1e-9) return Math.min(1, Math.max(0, (a + b) / 2));
	const integralA = a <= 0 ? 0 : a >= 1 ? a - 0.5 : a * a / 2;
	const integralB = b <= 0 ? 0 : b >= 1 ? b - 0.5 : b * b / 2;
	return (integralB - integralA) / (b - a);
}

/** Colour screen occupancy directly; antialias coverage was integrated before compositing. */
export function paintDensityContours(
	raster: Float32Array, pixels: Uint8ClampedArray, contrast: number, lut: Uint32Array,
): void {
	pixels.fill(0);
	for (let i = 0; i < raster.length; i++) {
		if (!(raster[i] > 0)) continue;
		const n = Math.min(1, raster[i] / MAX_DENSITY);
		const colour = lut[Math.round(255 * Math.pow(n, 0.45 * contrast))], o = i * 4;
		pixels[o] = colour & 0xff;
		pixels[o + 1] = (colour >> 8) & 0xff;
		pixels[o + 2] = (colour >> 16) & 0xff;
		pixels[o + 3] = Math.round(255 * (1 - Math.exp(-5 * Math.pow(n, 0.4))));
	}
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
	contrast: number, lut: Uint32Array,
): void {
	pixels.fill(0);
	const half = footprint / 2;
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
				const coverage = Math.min(y + 1, bottom) - Math.max(y, top);
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
