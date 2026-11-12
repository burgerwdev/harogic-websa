import { expect, it } from 'vitest';
import { glassMap } from '../ui/liquidGlass';

it('keeps the centre neutral and bends opposite lens rims in opposite directions', () => {
  const map = glassMap(240, 120, 20);
  const pixel = (x: number, y: number) => Array.from(map.pixels.slice((y * map.width + x) * 4, (y * map.width + x) * 4 + 4));
  expect(pixel(120, 60)).toEqual([128, 128, 128, 255]);
  expect(pixel(1, 60)[0]).toBeLessThan(128);
  expect(pixel(238, 60)[0]).toBeGreaterThan(128);
  expect(pixel(120, 1)[1]).toBeLessThan(128);
  expect(pixel(120, 118)[1]).toBeGreaterThan(128);
});

it('bounds map memory independently of viewport size', () => {
  const map = glassMap(7680, 4320, 22);
  expect(map.width * map.height).toBeLessThanOrEqual(512 * 256);
  expect(map.pixels.length).toBe(map.width * map.height * 4);
});
