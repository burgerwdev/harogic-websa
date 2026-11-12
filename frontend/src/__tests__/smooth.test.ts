import { expect, it } from 'vitest';
import { sgSmooth } from '../dsp/smooth';

function reference(src: Float32Array, width: number, adaptive: boolean): Float32Array {
  const gradients = Array.from(src.slice(1, -1), (_, i) => Math.abs(src[i + 2] - src[i]))
    .filter(Number.isFinite).sort((a, b) => a - b);
  const threshold = adaptive && src.length > 20 ? gradients[Math.floor(gradients.length * 0.9)] ?? Infinity : Infinity;
  return Float32Array.from(src, (_, i) => {
    const window = i > 0 && i + 1 < src.length && Math.abs(src[i + 1] - src[i - 1]) > threshold ? Math.min(3, width) : width;
    const half = (window - 1) >> 1, first = Math.max(0, i - half), end = Math.min(src.length - 1, i + half);
    if (end - first < 2) return src[i];
    // Solve the quadratic least-squares normal equations independently.
    const matrix = Array.from({ length: 3 }, () => [0, 0, 0, 0]);
    for (let j = first; j <= end; j++) {
      if (!Number.isFinite(src[j])) return NaN;
      for (let row = 0; row < 3; row++) {
        for (let col = 0; col < 3; col++) matrix[row][col] += (j - i) ** (row + col);
        matrix[row][3] += (j - i) ** row * src[j];
      }
    }
    for (let col = 0; col < 3; col++) {
      const pivot = matrix[col][col];
      for (let j = col; j < 4; j++) matrix[col][j] /= pivot;
      for (let row = 0; row < 3; row++) if (row !== col) {
        const factor = matrix[row][col];
        for (let j = col; j < 4; j++) matrix[row][j] -= factor * matrix[col][j];
      }
    }
    return matrix[0][3];
  });
}

it('agrees with independent quadratic fits and sorted gradient ranks at edges and carriers', () => {
  let seed = 91;
  for (const length of [2, 7, 21, 257, 1024]) {
    const samples = Float32Array.from({ length }, (_, i) => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return -120 + (seed / 2 ** 32) * 10 + (i % 113 === 0 ? 70 : 0);
    });
    const original = samples.slice();
    for (const adaptive of [false, true]) for (const width of [3, 5, 9, 17]) {
      const actual = sgSmooth(samples, width, adaptive), expected = reference(samples, width, adaptive);
      for (let i = 0; i < length; i++) expect(actual[i]).toBeCloseTo(expected[i], 4);
    }
    expect(samples).toEqual(original);
  }
});

it('preserves gaps and ignores missing gradients when choosing the edge threshold', () => {
  const samples = Float32Array.from({ length: 257 }, (_, i) => i < 127 ? -110 : -40);
  samples[20] = NaN; samples[180] = NaN;
  const actual = sgSmooth(samples, 9, true), expected = reference(samples, 9, true);
  expect(actual).toEqual(expected);
  expect(actual[126]).toBe(-110); expect(actual[127]).toBe(-40);
  expect([...sgSmooth(new Float32Array(25).fill(NaN), 9, true)].every(Number.isNaN)).toBe(true);
});

it('handles flat and repeated gradients on the largest display grid', () => {
  const flat = new Float32Array(65536).fill(-110);
  expect(sgSmooth(flat, 9, true)).toEqual(flat);
  const ramp = Float32Array.from({ length: 65536 }, (_, i) => -120 + i / 1024);
  expect(sgSmooth(ramp, 9, true)).toEqual(ramp);
});
