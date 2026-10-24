/** Frequency geometry shared by traces and bounded display layers. No interpolation. */
export interface AlignedSpectrum {
	freq: Float64Array;
	spec: Float32Array;
	/** The source coverage differs from the advertised display window. */
	shifted: boolean;
}

export function lowerBound(freq: ArrayLike<number>, hz: number): number {
	let lo = 0, hi = freq.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (freq[mid] < hz) lo = mid + 1; else hi = mid;
	}
	return lo;
}

/** Include one neighbour on either side so clipping retains boundary line segments. */
export function visibleBinRange(freq: ArrayLike<number>, lo: number, hi: number): [number, number] {
	const end = lowerBound(freq, hi);
	return [Math.max(0, lowerBound(freq, lo) - 1), Math.min(freq.length, end + (freq[end] === hi ? 2 : 1))];
}

export function sameFrequencyGrid(a: ArrayLike<number> | null, b: ArrayLike<number>): boolean {
	if (!a || a.length !== b.length) return false;
	if (a === b) return true;
	for (let i = 0; i < b.length; i++) if (a[i] !== b[i]) return false;
	return true;
}

export function alignToDisplayWindow(
	freq: ArrayLike<number>, spec: ArrayLike<number>, lo: number, hi: number,
): AlignedSpectrum {
	const f = freq instanceof Float64Array ? freq : Float64Array.from(freq);
	const p = spec instanceof Float32Array ? spec : Float32Array.from(spec);
	// Keep native bin centres, levels and NaNs. The renderer clips by Hz; an uncovered
	// display edge stays empty instead of manufacturing samples on a new uniform grid.
	return { freq: f, spec: p, shifted: f.length > 0 && (f[0] !== lo || f[f.length - 1] !== hi) };
}

/** Peak projection for coarse display layers only; traces retain the source arrays.
 * Empty columns stay NaN. Work is O(source + columns), never source * amplitude bins.
 */
export function projectSpectrum(
	freq: ArrayLike<number>, levels: ArrayLike<number>, lo: number, hi: number, out: Float32Array,
): Float32Array {
	out.fill(NaN);
	if (!(hi > lo)) return out;
	const scale = out.length / (hi - lo);
	for (let i = lowerBound(freq, lo); i < Math.min(freq.length, levels.length) && freq[i] <= hi; i++) {
		const v = levels[i];
		const x = Math.min(out.length - 1, Math.floor((freq[i] - lo) * scale));
		// A missing native sample breaks its coarse column too; do not bridge a gap.
		if (!Number.isFinite(v)) { out[x] = Infinity; continue; }
		if (out[x] !== Infinity && (!Number.isFinite(out[x]) || v > out[x])) out[x] = v;
	}
	for (let x = 0; x < out.length; x++) if (out[x] === Infinity) out[x] = NaN;
	return out;
}
