// Synthetic test data: simulate spectrum-analyzer traces (no device dependency)
// - synthCW: a single CW signal (Gaussian peak) + noise floor
// - synthBandpass: bandpass filter response (passband + deep valleys on both sides + flat stopbands) — valley-finding test scenario

export function synthCW(pts = 1000, opts: { peakIdx?: number; peakDb?: number; floor?: number; noiseAmp?: number } = {}) {
  const { peakIdx = 500, peakDb = -20, floor = -90, noiseAmp = 0.5 } = opts;
  const p = new Float32Array(pts);
  for (let i = 0; i < pts; i++) {
    const d = (i - peakIdx) / 30;          // Gaussian width ~30 bins
    const gauss = Math.exp(-(d * d));
    p[i] = floor + (peakDb - floor) * gauss + (Math.random() - 0.5) * noiseAmp;
  }
  return p;
}

// Bandpass: passband in [lo, hi] at -20dB, stopbands on both sides at -85dB, each with a deep valley (local minimum),
// with a sigmoid transition at the edges — matches a real filter's "deep valleys flanking the passband" measurement
export function synthBandpass(pts = 1000, opts: { lo?: number; hi?: number; passDb?: number; stopDb?: number; dipDb?: number } = {}) {
  const { lo = 0.3, hi = 0.7, passDb = -20, stopDb = -85, dipDb = -110 } = opts;
  const p = new Float32Array(pts);
  const edge = 12;   // transition-band width (bins)
  const dips = [0.15, 0.85];   // deep-valley positions on both sides (normalized x)
  for (let i = 0; i < pts; i++) {
    const x = i / pts;
    // sigmoid transition on both sides
    const rise = 1 / (1 + Math.exp(-(x - lo) * 4 * edge));
    const fall = 1 / (1 + Math.exp((x - hi) * 4 * edge));
    const resp = stopDb + (passDb - stopDb) * Math.min(rise, fall);
    // Deep valley: Gaussian trough (width ~12 bins, local minimum)
    let dip = 0;
    for (const dx of dips) {
      const d = (x - dx) * pts / 2;   // sharp narrow trough (adjacent bin diff >3dB, matching the real filter trough)
      dip = Math.min(dip, (dipDb - stopDb) * Math.exp(-(d * d)));
    }
    p[i] = resp + dip + (Math.random() - 0.5) * 0.4;
  }
  return p;
}

// Double peak (two isolated signals, for peak de-dup / Excursion)
export function synthTwoPeaks(pts = 1000) {
  const p = new Float32Array(pts);
  for (let i = 0; i < pts; i++) {
    let v = -95;
    for (const c of [{ idx: 300, db: -30 }, { idx: 650, db: -25 }]) {
      const d = (i - c.idx) / 12;
      v = Math.max(v, -95 + (c.db + 95) * Math.exp(-(d * d)));
    }
    p[i] = v;
  }
  return p;
}
