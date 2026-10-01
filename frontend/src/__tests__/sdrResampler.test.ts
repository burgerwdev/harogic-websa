import { describe, expect, it } from 'vitest';

import { StreamingPcm16Resampler } from '../audio/sdrResampler';

function pcm(values: number[]): ArrayBuffer {
  return new Int16Array(values).buffer;
}

describe('StreamingPcm16Resampler', () => {
  it('preserves continuous same-rate frames', () => {
    const resampler = new StreamingPcm16Resampler();
    const first = resampler.process(pcm([0, 8192, 16384]), 0, 3, 48000, 48000);
    const second = resampler.process(pcm([24576, 32767]), 0, 2, 48000, 48000);

    expect(Array.from(first)).toEqual([0, 0.25, 0.5]);
    expect(second[0]).toBeCloseTo(0.75);
    expect(second[1]).toBeCloseTo(32767 / 32768);
  });

  it('handles one-sample chunks without NaN or phase loss', () => {
    const resampler = new StreamingPcm16Resampler();
    const output: number[] = [];
    for (const value of [0, 4096, 8192, 12288, 16384]) {
      output.push(...resampler.process(pcm([value]), 0, 1, 48000, 24000));
    }

    expect(output).toEqual([0, 0.25, 0.5]);
    expect(output.every(Number.isFinite)).toBe(true);
  });

  it('reset discards interpolation history', () => {
    const resampler = new StreamingPcm16Resampler();
    expect(resampler.process(pcm([12000]), 0, 1, 48000, 48000)).toHaveLength(0);
    resampler.reset();
    expect(resampler.process(pcm([-12000]), 0, 1, 48000, 48000)).toHaveLength(0);
    const output = resampler.process(pcm([0]), 0, 1, 48000, 48000);
    expect(Array.from(output)).toEqual([-12000 / 32768, 0]);
  });
});
