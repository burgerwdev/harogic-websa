/** A fixed-coordinate release floor for a numeric raster; never accumulates new hits. */
export class DensityRelease {
	values = new Float32Array(0);
	private lastMs = NaN;
	private first = 0;
	private end = 0;

	/** Apply once per input revision. Times are milliseconds; half-life is seconds. */
	apply(raw: Float32Array, timeMs: number, halflifeSec: number, reset = false,
		first = 0, end = raw.length): Float32Array {
		if (this.values.length !== raw.length) {
			this.values = new Float32Array(raw.length);
			reset = true;
		}
		if (reset || !Number.isFinite(this.lastMs) || !(halflifeSec > 0)) {
			this.values.fill(0);
			this.values.set(raw.subarray(first, end), first);
		} else {
			first = Math.min(first, this.first); end = Math.max(end, this.end);
			const dt = Math.min(2, Math.max(0, (timeMs - this.lastMs) / 1000));
			const decay = Math.pow(2, -dt / halflifeSec);
			for (let i = first; i < end; i++) {
				this.values[i] = Math.max(raw[i], this.values[i] * decay);
			}
		}
		this.first = first; this.end = end;
		this.lastMs = timeMs;
		return this.values;
	}

	clear(): void {
		if (this.values.length) this.values = new Float32Array(0);
		this.lastMs = NaN;
		this.first = this.end = 0;
	}
}
