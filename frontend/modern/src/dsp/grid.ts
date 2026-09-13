/**
 * Align a spectrum frame to the display window it advertises.
 *
 * The SDR panadapter captures around its own centre (`capture_*`) and a hardware frequency
 * offset can push that away from the centre the user asked for. The frame header, however,
 * always carries the *display* window. Rebinning the capture grid onto the display grid:
 *
 *   - maps every bin by its true capture frequency (the freq array spans the capture range),
 *   - clips the result to the display range, so the user's centre lands at the canvas centre,
 *   - leaves NaN where the display window reaches past the capture range (the offset edge).
 *
 * The NaN is a real gap, not invented data: the trace renderer already breaks a line on a
 * non-finite sample, and GapFill is the user's explicit opt-in. Everything downstream
 * (markers, limits, density, waterfall) then works on one uniform frequency axis.
 */
export interface AlignedSpectrum {
	freq: Float64Array;
	spec: Float32Array;
	/** True when a rebin was actually needed (the capture grid differed from the window). */
	shifted: boolean;
}

export function alignToDisplayWindow(
	freq: ArrayLike<number>,
	spec: ArrayLike<number>,
	lo: number,
	hi: number,
): AlignedSpectrum {
	const asFreq = (): Float64Array => (freq instanceof Float64Array ? freq : Float64Array.from(freq));
	const asSpec = (): Float32Array => (spec instanceof Float32Array ? spec : Float32Array.from(spec));
	const n = freq.length;
	if (n < 2 || !(hi > lo)) return { freq: asFreq(), spec: asSpec(), shifted: false };
	const capLo = Number(freq[0]);
	const capHi = Number(freq[n - 1]);
	const capStep = (capHi - capLo) / (n - 1);
	// The display grid already *is* the capture grid (SWP/RTA, or an SDR capture the device
	// centred as asked): touching it could only add interpolation error. A real offset moves
	// the start by far more than one bin, so this test separates the two cases cleanly.
	if (!(capStep > 0) || Math.abs(capLo - lo) < capStep) {
		return { freq: asFreq(), spec: asSpec(), shifted: false };
	}
	const outFreq = new Float64Array(n);
	const outSpec = new Float32Array(n);
	const step = (hi - lo) / (n - 1);
	for (let i = 0; i < n; i++) {
		const f = lo + i * step;
		outFreq[i] = f;
		if (f < capLo - capStep * 0.5 || f > capHi + capStep * 0.5) {
			outSpec[i] = NaN;                    // outside the capture: leave a real gap
			continue;
		}
		const j = Math.min(n - 1, Math.max(0, (f - capLo) / capStep));
		const j0 = Math.floor(j);
		const j1 = Math.min(n - 1, j0 + 1);
		const t = j - j0;
		outSpec[i] = spec[j0] + (spec[j1] - spec[j0]) * t;
	}
	return { freq: outFreq, spec: outSpec, shifted: true };
}
