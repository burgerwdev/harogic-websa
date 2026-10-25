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
import { displayOffset, displayUnit } from '../ui/displayState';
import { commitLevelOffset } from '../ui/panels/refAmp';

beforeEach(() => {
  vi.useFakeTimers();
  resetAll();
  resetGraphMode();
  displayOffset.set(0);
  displayUnit.set('dBm');
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
  it('applies display defaults in SWP/RTA', () => {
    graphMode.confirm('std');
    expect(setDisplayRef('mode', -20)).toBe(true);
    expect(getDisplayRef()).toBe(-20);
    expect(setDisplayRef('preset', 0)).toBe(true);
    expect(getDisplayRef()).toBe(0);
  });

  it('refuses a background mode write in SDR (manual Ref must survive)', () => {
    graphMode.confirm('sdr');
    expect(setDisplayRef('user', -40)).toBe(true);
    expect(setDisplayRef('mode', 0)).toBe(false);
    expect(getDisplayRef()).toBe(-40);
    // An explicit Preset still resets it (the device resets its reference too).
    expect(setDisplayRef('preset', 0)).toBe(true);
    expect(getDisplayRef()).toBe(0);
  });

  it('lets the one-shot SDR fit write the scale', () => {
    graphMode.confirm('sdr');
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

  it('matches raw SDR acknowledgements after the display offset changes', () => {
    graphMode.confirm('sdr');
    commitLevelOffset(60);
    setDisplayRef('user', 100);
    expect(pendingDisplayRef()).toBe(100);
    commitLevelOffset(-60);
    expect(getDisplayRef()).toBe(-20);
    expect(pendingDisplayRef()).toBe(-20);
    noteDisplayRefReport(-20); // The display value is not the hardware request.
    expect(pendingDisplayRef()).toBe(-20);
    expect(displayRefDiverges()).toBe(true);
    noteDisplayRefReport(40);
    expect(pendingDisplayRef()).toBeNull();
    expect(getDisplayRef()).toBe(-20);
    expect(displayRefDiverges()).toBe(false);
  });

  it('rolls an SDR request back in the current offset domain at its original deadline', () => {
    graphMode.confirm('sdr');
    setDisplayRef('auto', -75);
    commitLevelOffset(60);
    setDisplayRef('user', 20);
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS - 1);
    commitLevelOffset(-60);
    expect(getDisplayRef()).toBe(-100);
    expect(pendingDisplayRef()).toBe(-100);
    vi.advanceTimersByTime(1);
    expect(pendingDisplayRef()).toBeNull();
    expect(getDisplayRef()).toBe(-135);
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
