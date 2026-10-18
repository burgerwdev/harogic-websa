import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GRAPH_MODE_TTL_MS,
  confirmGraphMode,
  currentGraphMode,
  graphMode,
  graphModeDiverges,
  pendingGraphMode,
  requestGraphMode,
  resetGraphMode,
  setGraphModeTimeoutHandler,
} from '../ui/graphMode';

beforeEach(() => {
  vi.useFakeTimers();
  resetGraphMode();
  graphMode.confirm('std');
  setGraphModeTimeoutHandler(null);
});

afterEach(() => {
  resetGraphMode();
  setGraphModeTimeoutHandler(null);
  vi.useRealTimers();
});

describe('graph mode handshake', () => {
  it('a request is pending until the matching STATUS confirms it', () => {
    requestGraphMode('rta');
    expect(pendingGraphMode()).toBe('rta');
    expect(currentGraphMode()).toBe('std');
    expect(graphModeDiverges()).toBe(true);
    expect(confirmGraphMode('rta')).toBe(true);
    expect(pendingGraphMode()).toBeNull();
    expect(currentGraphMode()).toBe('rta');
    expect(graphModeDiverges()).toBe(false);
  });

  it('a newer request supersedes the old one and an older reply is ignored', () => {
    requestGraphMode('rta');
    requestGraphMode('sdr');            // supersede: never blocked
    expect(pendingGraphMode()).toBe('sdr');
    expect(confirmGraphMode('rta')).toBe(false);   // reply for the old request
    expect(pendingGraphMode()).toBe('sdr');
    expect(confirmGraphMode('sdr')).toBe(true);
    expect(currentGraphMode()).toBe('sdr');
  });

  it('notifies instead of waiting silently when the request times out', () => {
    const seen: [string, string][] = [];
    setGraphModeTimeoutHandler((want, have) => seen.push([want, have]));
    requestGraphMode('rta');
    vi.advanceTimersByTime(GRAPH_MODE_TTL_MS - 1);
    expect(seen).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(seen).toEqual([['rta', 'std']]);
    expect(pendingGraphMode()).toBeNull();          // follow the backend again
    expect(currentGraphMode()).toBe('std');
  });

  it('a superseding request restarts the timeout (the old one cannot fire)', () => {
    const seen: string[] = [];
    setGraphModeTimeoutHandler((want) => seen.push(want));
    requestGraphMode('rta');
    vi.advanceTimersByTime(GRAPH_MODE_TTL_MS / 2);
    requestGraphMode('sdr');
    vi.advanceTimersByTime(GRAPH_MODE_TTL_MS / 2 + 1);   // old deadline passes
    expect(seen).toEqual([]);                            // the old request was replaced
    vi.advanceTimersByTime(GRAPH_MODE_TTL_MS);
    expect(seen).toEqual(['sdr']);
  });

  it('asking for the mode we are already in is a no-op', () => {
    requestGraphMode('std');
    expect(pendingGraphMode()).toBeNull();
    expect(graphModeDiverges()).toBe(false);
  });
});

describe('re-issued mode requests', () => {
	it('a re-click on the same target refreshes the TTL with the new watchdog', () => {
		// The old order: the re-click restarted the 8 s watchdog, but `set` kept the FIRST
		// intent's timestamp — the param TTL expired mid-window and the second watchdog
		// found `pending()` false, so the user waited silently and was never told.
		const seen: string[] = [];
		setGraphModeTimeoutHandler((want) => seen.push(want));

		requestGraphMode('sdr');
		vi.advanceTimersByTime(GRAPH_MODE_TTL_MS - 10);
		expect(pendingGraphMode()).toBe('sdr');
		requestGraphMode('sdr');                    // re-click 10 ms before the old deadline
		vi.advanceTimersByTime(GRAPH_MODE_TTL_MS - 20);
		expect(pendingGraphMode()).toBe('sdr');     // first TTL must not have expired the intent
		vi.advanceTimersByTime(20);
		expect(seen).toEqual(['sdr']);              // the new watchdog still notifies
	});
});
