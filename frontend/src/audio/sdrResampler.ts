// Streaming linear interpolation keeps each channel independent across packet boundaries.
export class StreamingFloatResampler {
  private position = 0;
  private previous = new Float32Array(2);
  private hasPrevious = false;

  reset(): void {
    this.position = 0;
    this.hasPrevious = false;
  }

  process(input: Float32Array, channels: number, sourceRate: number, outputRate: number): Float32Array {
    const frames = input.length / channels;
    if (sourceRate === outputRate) return input;
    if (frames === 0) return new Float32Array(0);
    const prefix = this.hasPrevious ? 1 : 0;
    const length = frames + prefix;
    const step = sourceRate / outputRate;
    const count = length < 2 ? 0 : Math.max(0, Math.floor((length - 1 - this.position) / step) + 1);
    const output = new Float32Array(count * channels);
    for (let k = 0; k < count; k++) {
      const position = this.position + k * step;
      const index = Math.min(Math.floor(position), length - 2);
      const fraction = position - index;
      for (let channel = 0; channel < channels; channel++) {
        const left = index < prefix ? this.previous[channel] : input[(index - prefix) * channels + channel];
        const right = input[(index + 1 - prefix) * channels + channel];
        output[k * channels + channel] = left + (right - left) * fraction;
      }
    }
    this.position += count * step - (length - 1);
    for (let channel = 0; channel < channels; channel++) {
      this.previous[channel] = input[(frames - 1) * channels + channel];
    }
    this.hasPrevious = true;
    return output;
  }
}
