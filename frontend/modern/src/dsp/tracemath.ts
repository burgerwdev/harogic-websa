// Trace math: operate on trace 1 (A) with trace 2 (B) in dB domain.
// A-B  : dB difference (can be negative) — e.g. isolation / gain vs reference
// A+B  : power-domain sum 10*log10(10^(A/10) + 10^(B/10)) — combined power
export type TraceMathOp = 'OFF' | 'A-B' | 'A+B';

export function subtractDb(a: Float32Array, b: Float32Array, out: Float32Array): void {
  const n = Math.min(a.length, b.length, out.length);
  for (let i = 0; i < n; i++) {
    out[i] = (isFinite(a[i]) && isFinite(b[i])) ? a[i] - b[i] : NaN;
  }
}

export function addPowerDb(a: Float32Array, b: Float32Array, out: Float32Array): void {
  const n = Math.min(a.length, b.length, out.length);
  for (let i = 0; i < n; i++) {
    if (!isFinite(a[i]) || !isFinite(b[i])) { out[i] = NaN; continue; }
    out[i] = 10 * Math.log10(Math.pow(10, a[i] / 10) + Math.pow(10, b[i] / 10));
  }
}

export function applyTraceMath(a: Float32Array, b: Float32Array, op: TraceMathOp): void {
  if (op === 'A-B') subtractDb(a, b, a);
  else if (op === 'A+B') addPowerDb(a, b, a);
}
