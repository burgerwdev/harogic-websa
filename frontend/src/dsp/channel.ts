// Channel measurements on a swept trace: channel power, occupied bandwidth and ACPR.
//
// Bin semantics: a spectrum-analyzer bin already holds the power measured inside one
// resolution bandwidth, so the power inside a band is the plain sum of its bin powers
// (no extra RBW weighting). Pure logic only, so it can be unit tested directly.

export interface AcprSetup {
  centerHz: number;
  channelBw: number;
  acpOffset: number;   // adjacent channel centre offset from the main channel centre
  acpBw: number;       // adjacent channel bandwidth
}
export interface ObwResult { bw: number; low: number; high: number; totalDbm: number; }
export interface AcprResult {
  mainDbm: number | null;
  lowerDbm: number | null;
  upperDbm: number | null;
  lowerDbc: number | null;
  upperDbc: number | null;
}

function mw(dbm: number): number {
  return Math.pow(10, dbm / 10);
}

function usable(freqs: ArrayLike<number>, levels: ArrayLike<number>, count?: number): number {
  const n = count === undefined
    ? Math.min(freqs.length, levels.length)
    : Math.min(count, freqs.length, levels.length);
  return n;
}

/** Index range [lo, hi) of bins with centre inside [f0-bw/2, f0+bw/2]; null when empty. */
function bandRange(freqs: ArrayLike<number>, f0: number, bw: number, n: number): [number, number] | null {
  const lo = f0 - bw / 2;
  const hi = f0 + bw / 2;
  let a = -1;
  let b = -1;
  for (let i = 0; i < n; i++) {
    const f = freqs[i];
    if (!Number.isFinite(f)) continue;
    if (f >= lo && f <= hi) {
      if (a < 0) a = i;
      b = i;
    }
  }
  return a < 0 ? null : [a, b + 1];
}

/** Total power (dBm) inside a band; null when the band holds no valid bin. */
export function bandPowerDbm(
  freqs: ArrayLike<number>,
  levels: ArrayLike<number>,
  f0: number,
  bw: number,
  count?: number,
): number | null {
  const n = usable(freqs, levels, count);
  const r = bandRange(freqs, f0, bw, n);
  if (!r) return null;
  let sum = 0;
  let used = 0;
  for (let i = r[0]; i < r[1]; i++) {
    const v = levels[i];
    if (!Number.isFinite(v)) continue;
    sum += mw(v);
    used++;
  }
  if (!used || !(sum > 0)) return null;
  return 10 * Math.log10(sum);
}

/**
 * Occupied bandwidth: the smallest band centred on f0 that holds `percent` of the total
 * power in the trace. The band grows symmetrically (one bin on each side per step, or only
 * the remaining side at the trace edge), which is the conventional carrier-centred OBW.
 */
export function occupiedBandwidth(
  freqs: ArrayLike<number>,
  levels: ArrayLike<number>,
  f0: number,
  percent: number,
  count?: number,
): ObwResult | null {
  const n = usable(freqs, levels, count);
  if (n < 2) return null;
  const levelAt = (i: number): number => {
    if (i < 0 || i >= n) return -Infinity;
    const v = levels[i];
    return Number.isFinite(v) ? v : -Infinity;
  };
  let total = 0;
  for (let i = 0; i < n; i++) {
    const v = levelAt(i);
    if (v > -Infinity) total += mw(v);
  }
  if (!(total > 0)) return null;
  let center = -1;
  let best = Infinity;
  for (let i = 0; i < n; i++) {
    if (levelAt(i) === -Infinity) continue;
    const d = Math.abs(freqs[i] - f0);
    if (d < best) { best = d; center = i; }
  }
  if (center < 0) return null;
  const target = total * Math.min(100, Math.max(1, percent)) / 100;
  let lo = center;
  let hi = center;
  let acc = mw(levelAt(center));
  while (acc < target && (lo > 0 || hi < n - 1)) {
    if (lo > 0) { lo--; acc += mw(levelAt(lo)); }
    if (hi < n - 1) { hi++; acc += mw(levelAt(hi)); }
  }
  const low = freqs[lo];
  const high = freqs[hi];
  return { bw: high - low, low, high, totalDbm: 10 * Math.log10(total) };
}

/** Channel power plus the lower/upper adjacent channel ratios (dBc). */
export function acpr(
  freqs: ArrayLike<number>,
  levels: ArrayLike<number>,
  setup: AcprSetup,
  count?: number,
): AcprResult | null {
  const main = bandPowerDbm(freqs, levels, setup.centerHz, setup.channelBw, count);
  if (main === null) return null;
  const lower = bandPowerDbm(freqs, levels, setup.centerHz - setup.acpOffset, setup.acpBw, count);
  const upper = bandPowerDbm(freqs, levels, setup.centerHz + setup.acpOffset, setup.acpBw, count);
  return {
    mainDbm: main,
    lowerDbm: lower,
    upperDbm: upper,
    lowerDbc: lower === null ? null : main - lower,
    upperDbc: upper === null ? null : main - upper,
  };
}
