import { describe, expect, it } from 'vitest';
import { channelPowerDbm } from '../sdr/squelchLevel';

describe('SDR tuned-channel squelch input', () => {
  it('ignores an off-channel carrier and missing bins', () => {
    const f = new Float64Array([99.998e6, 99.999e6, 100e6, 100.001e6, 100.002e6, 100.003e6]);
    const p = new Float32Array([-99, -98, -40, -97, -96, -12]);
    expect(channelPowerDbm(f, p, 100e6)).toBe(-40);
    expect(channelPowerDbm(f, p, 100.003e6)).toBe(-12);
    p[2] = NaN;
    expect(channelPowerDbm(f, p, 100e6)).toBe(-97);
    expect(channelPowerDbm(f, p, 101e6)).toBeNull();
    expect(channelPowerDbm(f.subarray(0, 1), p.subarray(0, 1), 100e6)).toBeNull();
  });
});
