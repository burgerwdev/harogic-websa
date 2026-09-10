import { describe, expect, it } from 'vitest';
import { activeIndex } from '../ui/railMath';

describe('activeIndex (control rail scroll spy)', () => {
  const tops = [0, 100, 250, 400];

  // boundaries are asserted with tolerance 0; the default 8 px is covered separately
  it('reports the first group at the top', () => {
    expect(activeIndex(tops, 0, 0)).toBe(0);
    expect(activeIndex(tops, 99, 0)).toBe(0);
  });

  it('reports the last group whose top has passed', () => {
    expect(activeIndex(tops, 100, 0)).toBe(1);
    expect(activeIndex(tops, 249, 0)).toBe(1);
    expect(activeIndex(tops, 250, 0)).toBe(2);
    expect(activeIndex(tops, 400, 0)).toBe(3);
  });

  it('clamps to the last group past the end of the list', () => {
    expect(activeIndex(tops, 5000, 0)).toBe(3);
  });

  it('uses the tolerance so a group activates just before its top reaches the edge', () => {
    expect(activeIndex(tops, 95, 0)).toBe(0);
    expect(activeIndex(tops, 95, 8)).toBe(1);
  });

  it('returns -1 when there are no groups', () => {
    expect(activeIndex([], 0)).toBe(-1);
  });

  it('handles a single group', () => {
    expect(activeIndex([30], 0)).toBe(0);
    expect(activeIndex([30], 999)).toBe(0);
  });
});
