import { describe, expect, it, beforeEach } from 'vitest';
import * as S from '../core/store';
import { fmtLevel, fromDisplayLevel, toDisplayLevel, unitOffsetDb } from '../core/level';

describe('amplitude unit conversion (50 ohm)', () => {
  beforeEach(() => {
    S.setLevelUnit('dBm');
    S.setDisplayOffset(0);
  });

  it('uses the standard dBm offsets', () => {
    expect(unitOffsetDb('dBm')).toBe(0);
    expect(unitOffsetDb('dBmV')).toBeCloseTo(46.99, 6);
    expect(unitOffsetDb('dBuV')).toBeCloseTo(106.99, 6);
    expect(unitOffsetDb('dBV')).toBeCloseTo(-13.01, 6);
  });

  it('converts instrument dBm to the selected unit', () => {
    S.setLevelUnit('dBuV');
    expect(toDisplayLevel(-20)).toBeCloseTo(86.99, 6);
    S.setLevelUnit('dBmV');
    expect(toDisplayLevel(-20)).toBeCloseTo(26.99, 6);
    S.setLevelUnit('dBV');
    expect(toDisplayLevel(-20)).toBeCloseTo(-33.01, 6);
  });

  it('round-trips a typed value back to dBm', () => {
    S.setLevelUnit('dBuV');
    expect(fromDisplayLevel(toDisplayLevel(-33.5))).toBeCloseTo(-33.5, 9);
  });

  it('adds the external gain/loss offset to readouts', () => {
    S.setDisplayOffset(10);
    expect(toDisplayLevel(-20)).toBeCloseTo(-10, 9);
    expect(fromDisplayLevel(-10)).toBeCloseTo(-20, 9);
    S.setLevelUnit('dBmV');
    expect(toDisplayLevel(-20)).toBeCloseTo(36.99, 6);   // offset + unit both applied
  });

  it('formats with the active unit and keeps differences unit free', () => {
    expect(fmtLevel(-22.463)).toBe('-22.46 dBm');
    S.setLevelUnit('dBuV');
    expect(fmtLevel(-22.463)).toBe('84.53 dBuV');
    S.setDisplayOffset(-3);
    expect(fmtLevel(-22.463, 1)).toBe('81.5 dBuV');
    expect(fmtLevel(NaN)).toBe('-');
  });
});
