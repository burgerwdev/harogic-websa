import { expect, it } from 'vitest';
import { WaterfallProjectionCache } from '../render/waterfallProjection';

it('retains independent codes for recolouring and invalidates every viewport dimension', () => {
  const cache = new WaterfallProjectionCache(64);
  const row = new Uint16Array(32), scratch = Uint16Array.of(0, 65000, 1, 32000);
  expect(cache.get(row, 4, 10, 20)).toBeUndefined();
  cache.put(row, scratch); scratch.fill(9);
  expect([...cache.get(row, 4, 10, 20)!]).toEqual([0, 65000, 1, 32000]);
  for (const [width, lo, hi] of [[8, 10, 20], [8, 11, 20], [8, 11, 21]]) {
    expect(cache.get(row, width, lo, hi)).toBeUndefined();
    expect(cache.bytes).toBe(0);
    cache.put(row, new Uint16Array(width));
  }
  cache.clear(); expect(cache.bytes).toBe(0);
  expect(cache.get(row, 8, 11, 21)).toBeUndefined();
});

it('bounds projection storage and bypasses small or oversized source rows', () => {
  const cache = new WaterfallProjectionCache(16);
  const rows = Array.from({ length: 20 }, () => new Uint16Array(64));
  for (const row of rows) {
    cache.get(row, 4, 0, 100); cache.put(row, new Uint16Array(4));
    expect(cache.bytes).toBeLessThanOrEqual(16);
  }
  expect(cache.get(rows[0], 4, 0, 100)).toBeUndefined();
  expect(cache.get(rows.at(-1)!, 4, 0, 100)).toHaveLength(4);
  cache.put(rows.at(-1)!, new Uint16Array(4)); expect(cache.bytes).toBe(16);
  cache.clear();
  cache.put(new Uint16Array(4), new Uint16Array(4));
  cache.put(rows[0], new Uint16Array(9)); expect(cache.bytes).toBe(0);
});
