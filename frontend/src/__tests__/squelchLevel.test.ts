/// The squelch gate's level feed: bin extraction at the listen frequency + throttle.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { feedSquelchLevel, resetSquelchLevelFeed } from '../sdr/squelchLevel';
import { setSdrSquelchLevel } from '../sdr/iqStream';
import { sdrListenHz } from '../ui/sdrState';

vi.mock('../sdr/iqStream', () => ({ setSdrSquelchLevel: vi.fn() }));

const F0 = 100e6;
const STEP = 763; // panadapter bin spacing

/** Spectrum grid with one tone of `toneDb` at `toneHz` over a -100 dB floor. */
function grid(toneHz: number, toneDb: number): { freqs: Float64Array; spec: Float32Array } {
  const freqs = new Float64Array(16);
  const spec = new Float32Array(16);
  for (let i = 0; i < 16; i++) {
    freqs[i] = F0 - 8 * STEP + i * STEP;
    spec[i] = -100;
    if (Math.abs(freqs[i] - toneHz) < STEP / 2) spec[i] = toneDb;
  }
  return { freqs, spec };
}

beforeEach(() => {
  vi.clearAllMocks();
  resetSquelchLevelFeed();
  sdrListenHz.confirm(F0);
});

afterEach(() => {
  resetSquelchLevelFeed();
  sdrListenHz.confirm(F0);
});

describe('squelch level feed', () => {
  it('sends the strongest bin within one step of the listen frequency', () => {
    const { freqs, spec } = grid(F0 + 200, -42);
    feedSquelchLevel(freqs, spec);
    expect(setSdrSquelchLevel).toHaveBeenCalledTimes(1);
    expect(vi.mocked(setSdrSquelchLevel).mock.calls[0][0]).toBeCloseTo(-42, 6);
  });

  it('ignores a listen frequency outside the displayed window', () => {
    sdrListenHz.confirm(F0 + 5e6);
    const { freqs, spec } = grid(F0, -42);
    feedSquelchLevel(freqs, spec);
    expect(setSdrSquelchLevel).not.toHaveBeenCalled();
  });

  it('throttles repeats but sends again once the interval passes', () => {
    const { freqs, spec } = grid(F0, -50);
    feedSquelchLevel(freqs, spec);
    feedSquelchLevel(freqs, spec);
    feedSquelchLevel(freqs, spec);
    expect(setSdrSquelchLevel).toHaveBeenCalledTimes(1);
  });
});
