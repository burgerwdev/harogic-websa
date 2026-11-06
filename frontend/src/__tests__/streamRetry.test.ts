import { afterEach, expect, it, vi } from 'vitest';
import { StreamRetry } from '../sdr/streamRetry';

afterEach(() => vi.useRealTimers());

it('requires sustained valid delivery to reset the server-fault retry budget', () => {
  vi.useFakeTimers();
  const retry = new StreamRetry();
  expect(retry.delay(1011, 'subscriber source stream invalid')).toBe(1000);
  retry.progress(); vi.advanceTimersByTime(2000);
  expect(retry.delay(1013, 'slow consumer')).toBe(2000);
  retry.progress(); vi.advanceTimersByTime(2000);
  expect(retry.delay(1011, 'receiver failed')).toBe(4000);
  expect(retry.delay(1011, 'receiver failed')).toBeNull();
  retry.progress(); vi.advanceTimersByTime(30_000); retry.progress();
  expect(retry.delay(1011, 'receiver failed')).toBe(1000);
  expect(retry.delay(1002, 'invalid protocol')).toBeNull();
});

it('backs off transport outages without spending the receiver-fault budget', () => {
  const retry = new StreamRetry();
  expect(Array.from({ length: 6 }, () => retry.delay(1006, ''))).toEqual([500, 1000, 2000, 4000, 5000, 5000]);
  expect(retry.delay(1011, 'receiver failed')).toBe(1000);
  retry.progress(); expect(retry.delay(1012, 'restart')).toBe(500);
});
