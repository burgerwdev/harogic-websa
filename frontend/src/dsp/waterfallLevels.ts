import { percentileApprox } from './stats';

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
/** Count original observations, never rendered/interpolated pixels. Caller bounds
 * the indices in Hz and decodes the retained affine dBm codes; zero means NaN. */
export function observedFloor(length: number, at: (i: number) => number, quantile = 0.1): { floor: number; count: number } {
  bins.fill(0);
  let count = 0;
  for (let i = 0; i < length; i++) {
    const value = at(i);
    if (!Number.isFinite(value)) continue;
    bins[Math.max(0, Math.min(1023, Math.floor((value + 240) * 2)))]++;
    count++;
  }
  if (!count) return { floor: NaN, count: 0 };
  const rank = Math.floor((count - 1) * quantile);
  let sum = 0;
  for (let i = 0; i < bins.length; i++) {
    sum += bins[i];
    if (sum > rank) return { floor: i / 2 - 239.75, count };
  }
  return { floor: NaN, count };
}

/** Too few bins, mostly missing, or a lower decile >12 dB above the
 * acquisition floor: use the acquisition estimate. Broadband signals and noise
 * are not identifiable from amplitudes alone; this guard favours weak signals. */
export function chooseViewFloor(local: { floor: number; count: number }, expected: number, capture: number): number {
  return local.count >= 32 && local.count >= expected / 4 && local.floor <= capture + 12 ? local.floor : capture;
}
