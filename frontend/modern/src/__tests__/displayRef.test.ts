import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { resetAll } from '../core/params';
import {
  DISPLAY_REF_TTL_MS,
  displayRefDiverges,
  getDisplayRef,
  noteDisplayRefReport,
  pendingDisplayRef,
  reportedDisplayRef,
  setDisplayRef,
  setDisplayRefTimeoutHandler,
} from '../ui/displayRef';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import { sdrRefAuto } from '../ui/sdrState';

beforeEach(() => {
  vi.useFakeTimers();
  resetAll();
  resetGraphMode();
  setDisplayRefTimeoutHandler(null);
  graphMode.confirm('std');
  setDisplayRef('preset', 0);
});

afterEach(() => {
  setDisplayRefTimeoutHandler(null);
  resetGraphMode();
  vi.useRealTimers();
});

describe('display reference ownership', () => {
  it('applies display defaults in SWP/RTA even with the SDR auto-scale preference on', () => {
    sdrRefAuto.set(true);
    graphMode.confirm('std');
    expect(setDisplayRef('mode', -20)).toBe(true);
    expect(getDisplayRef()).toBe(-20);
    expect(setDisplayRef('preset', 0)).toBe(true);
    expect(getDisplayRef()).toBe(0);
  });

  it('refuses a background mode write in SDR (manual Ref must survive)', () => {
    graphMode.confirm('sdr');
    sdrRefAuto.set(false);              // manual: the user owns the scale
    expect(setDisplayRef('user', -40)).toBe(true);
    expect(setDisplayRef('mode', 0)).toBe(false);
    expect(getDisplayRef()).toBe(-40);
    // An explicit Preset still resets it (the device resets its reference too).
    expect(setDisplayRef('preset', 0)).toBe(true);
    expect(getDisplayRef()).toBe(0);
  });

  it('still lets the SDR auto-scale write while it owns the value', () => {
    graphMode.confirm('sdr');
    sdrRefAuto.set(true);
    expect(setDisplayRef('auto', -15)).toBe(true);
    expect(getDisplayRef()).toBe(-15);
  });

  it('acknowledges a user request only with a matching backend report', () => {
    graphMode.confirm('sdr');
    setDisplayRef('user', -40);
    expect(pendingDisplayRef()).toBe(-40);
    noteDisplayRefReport(-30);          // a stale report
    expect(pendingDisplayRef()).toBe(-40);
    expect(displayRefDiverges()).toBe(true);
    noteDisplayRefReport(-40);          // the matching one
    expect(pendingDisplayRef()).toBeNull();
    expect(reportedDisplayRef()).toBe(-40);
    expect(displayRefDiverges()).toBe(false);
  });

  it('supersedes an older request and notifies when the device never confirms', () => {
    const seen: number[] = [];
    setDisplayRefTimeoutHandler((want) => seen.push(want));
    graphMode.confirm('sdr');
    setDisplayRef('user', -40);
    setDisplayRef('user', -50);         // supersede
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS - 1);
    expect(seen).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(seen).toEqual([-50]);        // the newest request, not the replaced one
    expect(pendingDisplayRef()).toBeNull();
  });
});
