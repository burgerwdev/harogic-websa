import { describe, expect, it } from 'vitest';
import { levelCrossing } from '../dsp/levelCross';

const thr = -30;
describe('levelCrossing (swept software level trigger)', () => {
  it('detects a rising crossing (inclusive at the threshold)', () => {
    expect(levelCrossing(new Float32Array([-60, -40]), new Float32Array([-20, -40]), thr, 'rising')).toBe(0);
    expect(levelCrossing(new Float32Array([-60]), new Float32Array([-30]), thr, 'rising')).toBe(0);
    expect(levelCrossing(new Float32Array([-30]), new Float32Array([-20]), thr, 'rising')).toBe(-1);
  });

  it('detects a falling crossing', () => {
    expect(levelCrossing(new Float32Array([-20, -40]), new Float32Array([-60, -40]), thr, 'falling')).toBe(0);
    expect(levelCrossing(new Float32Array([-40]), new Float32Array([-20]), thr, 'falling')).toBe(-1);
  });

  it('double edge accepts either direction', () => {
    const prev = new Float32Array([-60, -60]);
    const cur = new Float32Array([-60, -20]);
    expect(levelCrossing(prev, cur, thr, 'double')).toBe(1);
    expect(levelCrossing(prev, cur, thr, 'rising')).toBe(1);
    const prev2 = new Float32Array([-20]);
    const cur2 = new Float32Array([-60]);
    expect(levelCrossing(prev2, cur2, thr, 'double')).toBe(0);
    expect(levelCrossing(prev2, cur2, thr, 'rising')).toBe(-1);
  });

  it('does not fire when the level stays on the same side', () => {
    const above = new Float32Array([-10, -10]);
    const below = new Float32Array([-80, -80]);
    expect(levelCrossing(above, above, thr, 'double')).toBe(-1);
    expect(levelCrossing(below, below, thr, 'double')).toBe(-1);
  });

  it('skips invalid samples and rejects a bad threshold', () => {
    const prev = new Float32Array([NaN, -60]);
    const cur = new Float32Array([-20, -20]);
    expect(levelCrossing(prev, cur, thr, 'rising')).toBe(1);
    expect(levelCrossing(prev, cur, NaN, 'rising')).toBe(-1);
  });

  it('reports the first crossing in frequency order', () => {
    const prev = new Float32Array([-60, -60, -60]);
    const cur = new Float32Array([-60, -10, -10]);
    expect(levelCrossing(prev, cur, thr, 'rising')).toBe(1);
  });
});
