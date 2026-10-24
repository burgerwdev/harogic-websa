import { describe, expect, it } from 'vitest';
import { StreamingFloatResampler } from '../audio/sdrResampler';

describe('StreamingFloatResampler', () => {
  it('preserves same-rate stereo frames without changing channel order', () => {
    const resampler = new StreamingFloatResampler();
    const samples = new Float32Array([0.25, -0.5, 0.75, -1]);
    expect(Array.from(resampler.process(samples, 2, 48000, 48000))).toEqual([0.25, -0.5, 0.75, -1]);
  });

  it('handles one-frame chunks without phase loss', () => {
    const resampler = new StreamingFloatResampler();
    const output: number[] = [];
    for (const value of [0, 0.125, 0.25, 0.375, 0.5]) {
      output.push(...resampler.process(new Float32Array([value]), 1, 48000, 24000));
    }
    expect(output).toEqual([0, 0.25, 0.5]);
  });

  it.each([[48000, 44100], [44100, 48000]])('keeps stereo phase continuous from %i to %i', (sourceRate, targetRate) => {
    const input = Float32Array.from({ length: 2000 }, (_, i) => i % 2 === 0 ? Math.sin(i / 30) : -Math.sin((i - 1) / 30));
    const whole = new StreamingFloatResampler().process(input, 2, sourceRate, targetRate);
    const streaming = new StreamingFloatResampler();
    const split: number[] = [];
    for (let start = 0; start < input.length; start += 14) {
      split.push(...streaming.process(input.subarray(start, start + 14), 2, sourceRate, targetRate));
    }
    expect(split.length).toBe(whole.length);
    for (let i = 0; i < whole.length; i += 2) {
      expect(split[i]).toBeCloseTo(whole[i], 5);
      expect(split[i + 1]).toBeCloseTo(-split[i], 5);
    }
  });

  it('reset discards interpolation history when codec geometry changes', () => {
    const resampler = new StreamingFloatResampler();
    resampler.process(new Float32Array([0.75, -0.75]), 2, 48000, 44100);
    resampler.reset();
    const output = resampler.process(new Float32Array([-0.5, -0.25, 0]), 1, 24000, 48000);
    expect(Array.from(output)).toEqual([-0.5, -0.375, -0.25, -0.125, 0]);
  });
});
