export class StreamingPcm16Resampler {
  private position = 0;
  private previous: number | null = null;

  reset(): void {
    this.position = 0;
    this.previous = null;
  }

  process(
    buffer: ArrayBuffer,
    offset: number,
    samples: number,
    sourceRate: number,
    outputRate: number,
  ): Float32Array {
    if (samples <= 0 || sourceRate <= 0 || outputRate <= 0) return new Float32Array(0);
    const pcm = new Int16Array(buffer, offset, samples);
    let input = new Float32Array(samples);
    for (let i = 0; i < samples; i++) input[i] = pcm[i] / 32768;

    if (this.previous === null && input.length === 1) {
      this.previous = input[0];
      return new Float32Array(0);
    }
    if (this.previous !== null) {
      const extended = new Float32Array(samples + 1);
      extended[0] = this.previous;
      extended.set(input, 1);
      input = extended;
    }

    const step = sourceRate / outputRate;
    const length = input.length;
    const outputCount = Math.max(0, Math.floor((length - 1 - this.position) / step) + 1);
    const output = new Float32Array(outputCount);
    for (let k = 0; k < outputCount; k++) {
      const position = this.position + k * step;
      const i0 = Math.max(0, Math.min(Math.floor(position), length - 2));
      const fraction = Math.min(1, Math.max(0, position - i0));
      output[k] = input[i0] * (1 - fraction) + input[i0 + 1] * fraction;
    }
    this.position = outputCount > 0
      ? this.position + outputCount * step - (length - 1)
      : this.position - (length - 1);
    this.previous = input[length - 1];
    return output;
  }
}
