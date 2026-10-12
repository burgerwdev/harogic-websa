import { describe, expect, it } from 'vitest';
import { sReport, sScalePct, S_SCALE_TICKS } from '../core/sReport';

describe('sReport (Ham S mapping: S9=-73dBm, 6dB per S unit)', () => {
  it('maps the contract edge values', () => {
    expect(sReport(-121)).toBe('S1');
    expect(sReport(-73)).toBe('S9');
    expect(sReport(-67)).toBe('S9+6dB');
    expect(sReport(-13)).toBe('S9+60dB');   // capped at +60
  });

  it('covers the in-between units and the floor', () => {
    expect(sReport(-115)).toBe('S2');
    expect(sReport(-91)).toBe('S6');
    expect(sReport(-97)).toBe('S5');
    expect(sReport(-85)).toBe('S7');
    expect(sReport(-79)).toBe('S8');
    expect(sReport(-140)).toBe('S0');
    expect(sReport(-200)).toBe('S0');
  });

  it('is finite-safe', () => {
    expect(sReport(NaN)).toBe('-');
    expect(sReport(Infinity)).toBe('-');
  });
});

describe('sScalePct (non-linear S-scale bar geometry)', () => {
  it('anchors the unit boundaries at their even positions', () => {
    expect(sScalePct(-121)).toBe(0);      // S1
    expect(sScalePct(-73)).toBe(80);      // S9
    expect(sScalePct(-13)).toBe(100);     // S9+60
    expect(sScalePct(-63)).toBeCloseTo(83.33, 2);   // S9+10
    expect(sScalePct(-43)).toBeCloseTo(90, 2);      // S9+30
  });

  it('interpolates inside a segment and clamps outside', () => {
    expect(sScalePct(-121)).toBe(0);
    expect(sScalePct(-124)).toBe(0);      // below S1 clamps
    expect(sScalePct(-200)).toBe(0);
    expect(sScalePct(-67)).toBeCloseTo(82, 2);   // 6 dB into the +60 segment
    expect(sScalePct(0)).toBe(100);       // above +60 clamps
  });

  it('exposes ascending tick labels for the bar', () => {
    const pcts = S_SCALE_TICKS.map((t) => t.pct);
    expect([...pcts].sort((a, b) => a - b)).toEqual(pcts);
    expect(S_SCALE_TICKS[0].label).toBe('S1');
    expect(S_SCALE_TICKS[S_SCALE_TICKS.length - 1].label).toBe('+60');
  });
});
