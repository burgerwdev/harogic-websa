import { describe, expect, it } from 'vitest';
import { sReport } from '../core/sReport';

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
