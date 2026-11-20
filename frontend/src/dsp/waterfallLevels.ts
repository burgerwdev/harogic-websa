import { percentileApprox } from './stats';

/** SDR++/Gqrx/OpenWebRX separate dB limits from a fixed colour ramp. Contrast
 * adjusts these limits, rather than brightening everything above a noise pivot.
 * The upper limit stays fixed when increasing contrast; lowering contrast
 * broadens the range with a small extension into quieter detail. */
export function waterfallDisplayBounds(lo: number, hi: number, floor: number, contrast: number): { lo: number; hi: number } {
  contrast = Number.isFinite(contrast) ? Math.max(0.5, Math.min(2, contrast)) : 1;
  const span = hi - lo;
  if (contrast >= 1) {
    const lift = Math.min(span * 5 / 55 * (contrast - 1), span * 0.8,
      Number.isFinite(floor) ? Math.max(0, floor - lo - span / 55) : span * 5 / 55);
    return { lo: lo + lift, hi };
  }
  const softness = 1 - contrast;
  const bottom = lo - span * 4 / 55 * softness;
  return { lo: bottom, hi: bottom + span * (1 + 50 / 55 * softness) };
}

/** Stable SDR colours: a strong carrier must not dim all the weaker stations.
 * Track the lower part of the distribution, leaving 6 dB of visible noise grain
 * and 49 dB for signals. Follow gain/noise changes over 1.5 s, independently of
 * the spectrum trace's vertical scale. No averaging of signal bins or time rows.
 */
export class WaterfallAutoLevels {
  private floor = NaN;
  private lastAt = 0;

  update(levels: ArrayLike<number>, now: number, reset = false): { lo: number; hi: number; offset: boolean } {
    let valid = false;
    for (let i = 0; i < levels.length; i++) {
      if (Number.isFinite(levels[i])) { valid = true; break; }
    }
    return this.follow(valid ? percentileApprox(levels, 0.3) : NaN, now, reset);
  }

  follow(measured: number, now: number, reset = false): { lo: number; hi: number; offset: boolean } {
    if (reset) this.floor = NaN;
    if (Number.isFinite(measured)) {
      const dt = Math.max(0, Math.min(1000, now - this.lastAt));
      const alpha = 1 - Math.exp(-dt / 1500);
      this.floor = Number.isFinite(this.floor) ? this.floor + alpha * (measured - this.floor) : measured;
    }
    this.lastAt = now;
    const lo = (Number.isFinite(this.floor) ? this.floor : -120) - 6;
    return { lo, hi: lo + 55, offset: false };
  }
}

const bins = new Uint32Array(1024);
function fillLevelHistogram(length: number, at: (i: number) => number): number {
  bins.fill(0);
  let count = 0;
  for (let i = 0; i < length; i++) {
    const value = at(i);
    if (!Number.isFinite(value)) continue;
    bins[Math.max(0, Math.min(1023, Math.floor((value + 240) * 2)))]++;
    count++;
  }
  return count;
}

function levelQuantile(count: number, quantile: number): number {
  if (!count) return NaN;
  const rank = Math.floor((count - 1) * quantile);
  let sum = 0;
  for (let i = 0; i < bins.length; i++) {
    sum += bins[i];
    if (sum > rank) return i / 2 - 239.75;
  }
  return NaN;
}

/** Estimate a quantile of finite dBm samples with half-dB histogram bins. */
export function observedFloor(length: number, at: (i: number) => number, quantile = 0.1): { floor: number; count: number } {
  const count = fillLevelHistogram(length, at);
  return { floor: levelQuantile(count, quantile), count };
}

/** Too few bins, mostly missing, or a lower decile >12 dB above the
 * acquisition floor: use the acquisition estimate. Broadband signals and noise
 * are not identifiable from amplitudes alone; this guard favours weak signals. */
export function chooseViewFloor(local: { floor: number; count: number }, expected: number, capture: number): number {
  return local.count >= 32 && local.count >= expected / 4 && local.floor <= capture + 12 ? local.floor : capture;
}

/** A display column can contain the loudest of many FFT bins. Account for that
 * rise in background noise, while keeping narrow or signal-filled views tied
 * to the tracked RF floor. The shorter window gives weak signals more colours. */
export function darkWaterfallBounds(floor: number, pixels: Uint16Array, level: { lo: number; step: number },
  binsPerPixel: number, contrast: number): { lo: number; hi: number } {
  const sampleCount = Math.min(512, pixels.length);
  const count = fillLevelHistogram(sampleCount, i => {
    const code = pixels[Math.floor((i + 0.5) * pixels.length / sampleCount)];
    return code ? level.lo + (code - 1) * level.step : NaN;
  });
  // In a magnified view even a broad station cannot raise black by more
  // than 3 dB. Peak-reduced columns allow progressively more noise lift.
  const maxLift = Math.min(16, 3 + 2 * Math.log2(Math.max(1, binsPerPixel)));
  let background = floor;
  let margin = 1 + 1 / Math.max(1, binsPerPixel);
  if (count >= 32) {
    const median = levelQuantile(count, 0.5);
    background += Math.min(maxLift, Math.max(0, median - floor));
    // The quiet half of the row measures noise scatter without treating
    // strong carriers as noise. Widen the black margin only on noisy rows.
    // Capping the median at the tracked background also protects a zoom
    // into an occupied passband. Half-dB histogram bins limit jitter.
    const scatter = Math.min(median, background) - levelQuantile(count, 0.25);
    margin = Math.max(margin, Math.min(6.5, 1.5 * scatter));
  }
  const lo = background + margin + (contrast - 1) * 3;
  // A carrier 6 dB above black reaches mid-palette. waterfallHeadroom
  // compresses the upper end so stronger carriers retain distinct colours.
  return { lo, hi: lo + 12 };
}
