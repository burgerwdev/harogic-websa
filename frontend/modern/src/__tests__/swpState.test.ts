import { describe, expect, it } from 'vitest';

import { resetAll } from '../core/params';
import {
  currentPoints,
  currentRBW,
  currentSpur,
  currentVBW,
  rbwMode,
  vbwMode,
} from '../ui/swpState';

describe('swpState slots', () => {
  it('registers every swept parameter under the swp scope', () => {
    for (const p of [rbwMode, vbwMode, currentRBW, currentVBW, currentPoints, currentSpur]) {
      expect(p.scope).toBe('swp');
    }
  });

  it('a user RBW mode wins until the backend confirms it', () => {
    rbwMode.confirm('auto');
    rbwMode.set('manual');
    expect(rbwMode.get()).toBe('manual');
    expect(rbwMode.pending()).toBe(true);
    rbwMode.confirm('manual');
    expect(rbwMode.get()).toBe('manual');
    expect(rbwMode.pending()).toBe(false);
  });

  it('resetAll(swp) drops a pending intent', () => {
    currentRBW.confirm(300e3);
    currentRBW.set(100e3);
    expect(currentRBW.get()).toBe(100e3);
    resetAll('swp');
    expect(currentRBW.get()).toBe(300e3);
  });
});
