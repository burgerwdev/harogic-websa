// Allocation-light approximate percentile for real-time display paths.
const HIST_MIN = -220;
const HIST_MAX = 80;
const HIST_BINS = 512;
const histogram = new Uint32Array(HIST_BINS);

export function percentileApprox(values: ArrayLike<number>, quantile: number): number {
  histogram.fill(0);
  let count = 0;
  for (let i = 0; i < values.length; i++) {
    const value = values[i];
    if (!isFinite(value)) continue;
    const normalized = (value - HIST_MIN) / (HIST_MAX - HIST_MIN);
    const bin = Math.max(0, Math.min(HIST_BINS - 1, Math.floor(normalized * HIST_BINS)));
    histogram[bin]++;
    count++;
  }
  if (!count) return HIST_MIN;
  const target = Math.max(0, Math.min(count - 1, Math.floor(count * quantile)));
  let cumulative = 0;
  for (let bin = 0; bin < HIST_BINS; bin++) {
    cumulative += histogram[bin];
    if (cumulative > target) {
      return HIST_MIN + (bin + 0.5) * (HIST_MAX - HIST_MIN) / HIST_BINS;
    }
  }
  return HIST_MAX;
}

/**
 * Reject implausible RTA frames: right after a reconfiguration the device can emit a
 * saturated packet (near full scale ≈ 0 dBm) before valid data arrives. Accumulating or
 * rendering such a frame makes an averaging trace decay slowly from the top of the graph.
 */
export function plausibleSpectrum(spec: ArrayLike<number>): boolean {
  if (spec.length < 8) return false;
  const floor = percentileApprox(spec, 0.3);
  const bulk = percentileApprox(spec, 0.9);
  // A valid trace has a low noise floor and only a minority of bins near full scale;
  // saturated/undefined packets fail both checks.
  return isFinite(floor) && floor <= -20 && isFinite(bulk) && bulk <= -15;
}
