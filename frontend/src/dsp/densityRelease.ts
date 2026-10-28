/** A fixed-coordinate release floor for a numeric raster; never accumulates new hits. */
export class DensityRelease {
	values = new Float32Array(0);
	private lastMs = NaN;

	/** Apply once per input revision. Times are milliseconds; half-life is seconds. */
	apply(raw: Float32Array, timeMs: number, halflifeSec: number, reset = false): Float32Array {
		if (this.values.length !== raw.length) {
			this.values = new Float32Array(raw.length);
			reset = true;
		}
		if (reset || !Number.isFinite(this.lastMs) || !(halflifeSec > 0)) {
			this.values.set(raw);
		} else {
			const dt = Math.min(2, Math.max(0, (timeMs - this.lastMs) / 1000));
			const decay = Math.pow(2, -dt / halflifeSec);
			for (let i = 0; i < raw.length; i++) {
				this.values[i] = Math.max(raw[i], this.values[i] * decay);
			}
		}
		this.lastMs = timeMs;
		return this.values;
	}

	clear(): void {
		if (this.values.length) this.values = new Float32Array(0);
		this.lastMs = NaN;
	}
}
