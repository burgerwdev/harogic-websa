import { describe, expect, it } from 'vitest';
import { acpr, bandPowerDbm, occupiedBandwidth } from '../dsp/channel';

// 100 bins, 10 kHz spacing, 1 MHz span: uniform grid unless stated otherwise.
const N = 100;
const SPACING = 10e3;
const freqs = new Float64Array(Array.from({ length: N }, (_, i) => 1e6 + i * SPACING));
const center = 1e6 + (N / 2) * SPACING;   // bin 50

function noise(level = -100): Float32Array {
  return new Float32Array(N).fill(level);
}
function withTone(i: number, level: number, base = -100): Float32Array {
  const a = noise(base);
  a[i] = level;
  return a;
}

describe('bandPowerDbm', () => {
  it('sums the bin powers inside the band', () => {
    // Two equal bins at -80 dBm -> -80 + 10*log10(2) = -76.99 dBm
    const lv = noise(-120);
    lv[40] = -80;
    lv[41] = -80;
    const p = bandPowerDbm(freqs, lv, freqs[40] + SPACING / 2, SPACING * 2)!;
    expect(p).toBeCloseTo(-80 + 10 * Math.log10(2), 6);
  });

  it('a dominant tone dominates a noisy band', () => {
    const p = bandPowerDbm(freqs, withTone(50, -20), center, 1e6)!;
    expect(p).toBeCloseTo(-20, 3);   // 100 bins of -100 dBm add only 0.0004 dB
  });

  it('returns null when the band holds no bin', () => {
    expect(bandPowerDbm(freqs, noise(), 5e9, 1e6)).toBeNull();
  });

  it('ignores invalid samples', () => {
    const lv = withTone(50, -20);
    lv[49] = NaN;
    lv[51] = NaN;
    expect(bandPowerDbm(freqs, lv, center, 1e6)).toBeCloseTo(-20, 3);
  });

  it('handles a band wider than the trace', () => {
    const p = bandPowerDbm(freqs, noise(-100), center, 1e9)!;
    expect(p).toBeCloseTo(-100 + 10 * Math.log10(N), 6);
  });
});

describe('occupiedBandwidth', () => {
  it('a single dominant tone occupies about one bin', () => {
    const obw = occupiedBandwidth(freqs, withTone(50, -20), center, 99)!;
    expect(obw).not.toBeNull();
    expect(obw.bw).toBeLessThanOrEqual(2 * SPACING);
    expect(obw.low).toBeLessThanOrEqual(freqs[50]);
    expect(obw.high).toBeGreaterThanOrEqual(freqs[50]);
  });

  it('two equal tones give an occupied bandwidth of their separation', () => {
    const lv = noise(-120);
    lv[45] = -20;
    lv[55] = -20;
    const obw = occupiedBandwidth(freqs, lv, center, 99)!;
    expect(obw.bw).toBeCloseTo(10 * SPACING, 6);
  });

  it('widens with the requested percentage', () => {
    const lv = withTone(50, -40, -60);   // tone only 20 dB above a flat floor
    const p90 = occupiedBandwidth(freqs, lv, center, 90)!;
    const p99 = occupiedBandwidth(freqs, lv, center, 99)!;
    expect(p99.bw).toBeGreaterThan(p90.bw);
  });

  it('reports the total power of the trace', () => {
    const obw = occupiedBandwidth(freqs, noise(-100), center, 99)!;
    expect(obw.totalDbm).toBeCloseTo(-100 + 10 * Math.log10(N), 6);
  });

  it('returns null for a degenerate trace', () => {
    expect(occupiedBandwidth(new Float64Array([1e6]), new Float32Array([-20]), 1e6, 99)).toBeNull();
    expect(occupiedBandwidth(freqs, new Float32Array(N).fill(NaN), center, 99)).toBeNull();
  });
});

describe('acpr', () => {
  const setup = { centerHz: center, channelBw: 100e3, acpOffset: 200e3, acpBw: 100e3 };

  it('reports main channel power and both adjacent ratios', () => {
    const lv = noise(-110);
    lv[50] = -20;                                  // main carrier at the centre bin
    for (let i = 25; i <= 35; i++) lv[i] = -90;    // lower adjacent channel (11 bins)
    for (let i = 65; i <= 75; i++) lv[i] = -90;    // upper adjacent channel (11 bins)
    const r = acpr(freqs, lv, setup)!;
    // Main: -20 dBm tone plus 10 bins of -110 dBm (negligible). Adjacent: 11 bins of -90 dBm.
    const adj = -90 + 10 * Math.log10(11);
    expect(r.mainDbm!).toBeCloseTo(-20, 3);
    expect(r.lowerDbm!).toBeCloseTo(adj, 6);
    expect(r.lowerDbc!).toBeCloseTo(-20 - adj, 6);
    expect(r.upperDbc!).toBeCloseTo(r.lowerDbc!, 9);
  });

  it('returns null when the main channel has no bin', () => {
    expect(acpr(freqs, noise(), { ...setup, centerHz: 5e9 })).toBeNull();
  });

  it('reports a null side when an adjacent band falls outside the trace', () => {
    const r = acpr(freqs, withTone(50, -20), { ...setup, acpOffset: 5e9 })!;
    expect(r.mainDbm).not.toBeNull();
    expect(r.lowerDbc).toBeNull();
    expect(r.upperDbc).toBeNull();
  });
});
