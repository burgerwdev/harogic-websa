/** Native-bin carrier evidence from raw frames, with bounded per-axis storage. */
export const CARRIER_PROMINENCE_DB = 8;
export const CARRIER_MIN_FRAMES = 3;
export const CARRIER_MIN_SPAN_MS = 120;
export const CARRIER_STALE_MS = 350;
const FRAMES_MAX = 16;
const UNIFORM_TOL = 1e-3;
const NEAR_BINS = 1;

export interface CarrierEvidence {
  carrierHz: number;
  bin: number;
  powerDb: number;
  prominenceDb: number;
  frames: number;
  firstMs: number;
  lastMs: number;
  binHz: number;
}

export interface CarrierCandidate {
  hz: number;
  quality: number;
  atMs: number;
}

export type CarrierMissReason =
  | 'no-evidence' | 'stale' | 'out-of-view' | 'absent' | 'ambiguous' | 'coarse-bins';
export interface CarrierHit { ok: true; candidate: CarrierEvidence }
export interface CarrierMiss { ok: false; reason: CarrierMissReason }
export interface FindCarrierOptions {
  hz: number;
  radiusHz: number;
  maxBinHz: number;
  now: number;
  // When bins are wider than maxBinHz, snap to the nearest local peak instead of
  // refusing. Off by default: fine carrier resolution stays honest unless a caller asks.
  coarseFallback?: boolean;
}

interface FrameRecord {
  /** Zero means no locally credible peak at this bin. */
  prominence: Float32Array;
  atMs: number;
}
interface History {
  identity: number | string;
  freq: Float64Array;
  binHz: number;
  head: number;
  used: number;
  records: FrameRecord[];
  powers: Float32Array;
}
let history: History | null = null;
let version = 0;

/** Changes when evidence is reset or its source axis changes. */
export function carrierEvidenceVersion(): number { return version; }

export function resetCarrierEvidence(): void {
  history = null;
  version++;
}

/** True when the current evidence axis is too coarse for fine carrier resolution. */
export function carrierAxisCoarse(maxBinHz: number): boolean {
  return history !== null && history.binHz > maxBinHz;
}

/** Invalid or changed native axes cannot retain evidence from a previous source. */
export function observeCarrierSpectrum(
  freq: Float64Array,
  powers: Float32Array,
  now: number,
  identity: number | string,
): boolean {
  const n = Math.min(freq.length, powers.length);
  let stepMin = Infinity, stepMax = 0;
  if (n < 8 || !Number.isFinite(now) || !Number.isFinite(freq[0])) {
    resetCarrierEvidence();
    return false;
  }
  for (let i = 1; i < n; i++) {
    const step = freq[i] - freq[i - 1];
    if (!Number.isFinite(step) || !(step > 0)) {
      resetCarrierEvidence();
      return false;
    }
    stepMin = Math.min(stepMin, step);
    stepMax = Math.max(stepMax, step);
  }
  if (stepMax - stepMin > UNIFORM_TOL * Math.max(stepMax, 1)) {
    resetCarrierEvidence();
    return false;
  }
  let h = history;
  if (!h || h.identity !== identity || h.freq.length !== n
    || Math.abs(h.freq[0] - freq[0]) >= 1e-6
    || Math.abs(h.freq[n - 1] - freq[n - 1]) >= 1e-6) {
    version++;
    h = history = {
      identity, freq: freq.slice(0, n), binHz: (stepMin + stepMax) / 2,
      head: -1, used: 0, powers: new Float32Array(n),
      records: Array.from({ length: FRAMES_MAX }, () => ({
        prominence: new Float32Array(n), atMs: NaN,
      })),
    };
  } else if (h.used && (now < h.records[h.head].atMs
    || now - h.records[h.head].atMs > CARRIER_STALE_MS)) {
    h.head = -1;
    h.used = 0;
    version++;
  } else if (h.used && now === h.records[h.head].atMs) {
    return false;
  }
  const slot = (h.head + 1) % FRAMES_MAX;
  const rec = h.records[slot];
  rec.prominence.fill(0);
  h.powers.set(powers.subarray(0, n));
  for (let i = 2; i < n - 2; i++) {
    const p = powers[i], left = powers[i - 1], right = powers[i + 1];
    if (!Number.isFinite(p) || !Number.isFinite(left) || !Number.isFinite(right)
      || !(p > left && p >= right)) continue;
    // Windowed carrier lobes may need four bins to reach both shoulders.
    let shoulders = true;
    for (let direction = -1; direction <= 1; direction += 2) {
      let previous = p, descended = false;
      for (let d = 1; d <= 4; d++) {
        const value = powers[i + direction * d];
        if (!Number.isFinite(value) || (value > previous && p - previous < 4)) break;
        previous = value;
        if (d >= 2 && p - value >= 4) { descended = true; break; }
      }
      if (!descended) { shoulders = false; break; }
    }
    if (!shoulders) continue;
    let leftSum = 0, rightSum = 0, leftCount = 0, rightCount = 0;
    for (let d = 3; d <= 8; d++) {
      if (i - d >= 0 && Number.isFinite(powers[i - d])) {
        leftSum += powers[i - d];
        leftCount++;
      }
      if (i + d < n && Number.isFinite(powers[i + d])) {
        rightSum += powers[i + d];
        rightCount++;
      }
    }
    if (leftCount < 2 || rightCount < 2) continue;
    const prominence = p - Math.max(leftSum / leftCount, rightSum / rightCount);
    if (prominence >= CARRIER_PROMINENCE_DB) rec.prominence[i] = prominence;
  }
  rec.atMs = now;
  h.head = slot;
  h.used = Math.min(h.used + 1, FRAMES_MAX);
  return true;
}

function localPeak(rec: FrameRecord, bin: number): number {
  let best = -1;
  for (let i = Math.max(0, bin - NEAR_BINS); i <= Math.min(rec.prominence.length - 1, bin + NEAR_BINS); i++) {
    if (rec.prominence[i] > 0 && (best < 0 || rec.prominence[i] > rec.prominence[best])) best = i;
  }
  return best;
}

/** Current local support for an already acquired native bin; never searches farther away. */
export function currentCarrierEvidence(hz: number, now: number, maxBinHz: number, coarseFallback = false): CarrierCandidate | null {
  const h = history;
  if (!h || !h.used || (h.binHz > maxBinHz && !coarseFallback)) return null;
  const rec = h.records[h.head];
  if (now < rec.atMs || now - rec.atMs >= CARRIER_STALE_MS) return null;
  const bin = Math.round((hz - h.freq[0]) / h.binHz);
  if (bin < 0 || bin >= h.freq.length || Math.abs(h.freq[bin] - hz) > 1e-6) return null;
  const peak = h.binHz > maxBinHz ? (rec.prominence[bin] > 0 ? bin : -1) : localPeak(rec, bin);
  return peak < 0 ? null : { hz, quality: Math.min(1, rec.prominence[peak] / 30), atMs: rec.atMs };
}

function trail(h: History, bin: number, now: number): CarrierEvidence | null {
  let count = 0, firstMs = h.records[h.head].atMs, minProm = Infinity;
  const lastMs = firstMs;
  for (let k = 0; k < h.used; k++) {
    const rec = h.records[(h.head - k + FRAMES_MAX) % FRAMES_MAX];
    if (now - rec.atMs > CARRIER_STALE_MS) break;
    const peak = localPeak(rec, bin);
    if (peak < 0) break;
    count++;
    firstMs = rec.atMs;
    minProm = Math.min(minProm, rec.prominence[peak]);
  }
  if (count < CARRIER_MIN_FRAMES || lastMs - firstMs < CARRIER_MIN_SPAN_MS) return null;
  return {
    carrierHz: h.freq[bin], bin, powerDb: h.powers[bin], prominenceDb: minProm,
    frames: count, firstMs, lastMs, binHz: h.binHz,
  };
}

/** Pure acquisition: nearest persistent local peak, with no hold or interpolation. */
export function findCarrierNear(options: FindCarrierOptions): CarrierHit | CarrierMiss {
  const h = history;
  if (!h || !h.used) return { ok: false, reason: 'no-evidence' };
  const { hz, radiusHz, maxBinHz, now } = options;
  if (!Number.isFinite(hz) || !(radiusHz > 0)) return { ok: false, reason: 'out-of-view' };
  if (now < h.records[h.head].atMs || now - h.records[h.head].atMs >= CARRIER_STALE_MS) return { ok: false, reason: 'stale' };
  const lo = Math.max(0, Math.ceil((hz - radiusHz - h.freq[0]) / h.binHz));
  const hi = Math.min(h.freq.length - 1, Math.floor((hz + radiusHz - h.freq[0]) / h.binHz));
  if (lo > hi) return { ok: false, reason: 'out-of-view' };
  const rec = h.records[h.head];
  if (h.binHz > maxBinHz) return options.coarseFallback ? coarsePeakFallback(h, rec, lo, hi, hz) : { ok: false, reason: 'coarse-bins' };
  let best: CarrierEvidence | null = null, bestDistance = Infinity, tied = false;
  for (let bin = lo; bin <= hi; bin++) {
    if (!rec.prominence[bin]) continue;
    const candidate = trail(h, bin, now);
    if (!candidate) continue;
    const distance = Math.abs(candidate.carrierHz - hz);
    if (distance < bestDistance - 1e-6) {
      best = candidate;
      bestDistance = distance;
      tied = false;
    } else if (Math.abs(distance - bestDistance) <= 1e-6) tied = true;
  }
  if (!best) return { ok: false, reason: 'absent' };
  return tied ? { ok: false, reason: 'ambiguous' } : { ok: true, candidate: best };
}

// Bins wider than the fine gate cannot resolve a carrier lobe, so fall back to the nearest
// credible local peak inside the pointer radius. Only a genuine tie is refused: the
// runner-up must sit within one bin of the nearest distance. Farther peaks never veto.
function coarsePeakFallback(h: History, rec: FrameRecord, lo: number, hi: number, hz: number): CarrierHit | CarrierMiss {
  let best = -1, bestDist = Infinity, secondDist = Infinity;
  for (let bin = lo; bin <= hi; bin++) {
    if (!(rec.prominence[bin] > 0)) continue;
    const dist = Math.abs(h.freq[bin] - hz);
    if (dist < bestDist) { secondDist = bestDist; bestDist = dist; best = bin; }
    else if (dist < secondDist) secondDist = dist;
  }
  if (best < 0) return { ok: false, reason: 'absent' };
  if (secondDist - bestDist < h.binHz) return { ok: false, reason: 'ambiguous' };
  return { ok: true, candidate: {
    carrierHz: h.freq[best], bin: best, powerDb: h.powers[best], prominenceDb: rec.prominence[best],
    frames: 1, firstMs: rec.atMs, lastMs: rec.atMs, binHz: h.binHz,
  } };
}

export function findCarrierCandidate(hz: number, maxOffsetHz: number, now: number, maxBinHz: number, coarseFallback = false): CarrierCandidate | null {
  const result = findCarrierNear({ hz, radiusHz: maxOffsetHz, now, maxBinHz, coarseFallback });
  if (!result.ok) return null;
  const c = result.candidate;
  return { hz: c.carrierHz, quality: Math.min(1, c.prominenceDb / 30), atMs: c.lastMs };
}
