/* SDR playback processor. Input samples are already resampled to the AudioContext rate. */
class SdrAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Float32Array(Math.max(8192, Math.floor(sampleRate * 2)));
    this.writePos = 0;
    this.available = 0;
    this.enabled = false;
    this.playing = false;
    this.primed = false;
    this.fade = 0;
    this.lastSample = 0;
    this.tailGain = 0;
    this.fadeStep = 1 / Math.max(1, sampleRate * 0.01);
    this.initialThreshold = Math.max(128, Math.floor(sampleRate * 0.08));
    this.resumeThreshold = Math.max(128, Math.floor(sampleRate * 0.02));
    this.reportCountdown = 0;
    this.underruns = 0;
    this.port.onmessage = (event) => {
      const message = event.data || {};
      if (message.type === 'samples' && message.samples) {
        this.push(message.samples);
      } else if (message.type === 'reset') {
        this.reset();
      } else if (message.type === 'enabled') {
        this.enabled = Boolean(message.value);
        if (!this.enabled) {
          this.tailGain = Math.max(this.tailGain, this.fade);
          this.available = 0;
          this.playing = false;
          this.primed = false;
          this.fade = 0;
          this.underruns = 0;
        }
      }
    };
    this.port.postMessage({ type: 'ready' });
  }

  reset() {
    this.tailGain = this.fade;
    this.writePos = 0;
    this.available = 0;
    this.playing = false;
    this.primed = false;
    this.fade = 0;
    this.underruns = 0;
  }

  push(samples) {
    for (let i = 0; i < samples.length; i++) {
      this.ring[this.writePos] = samples[i];
      this.writePos = (this.writePos + 1) % this.ring.length;
      if (this.available < this.ring.length) this.available++;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0] && outputs[0][0];
    if (!out) return true;
    out.fill(0);
    for (let i = 0; i < out.length; i++) {
      if (this.tailGain > 0) {
        out[i] = this.lastSample * this.tailGain;
        this.tailGain = Math.max(0, this.tailGain - this.fadeStep);
        continue;
      }
      if (!this.enabled) continue;
      if (!this.playing) {
        const threshold = this.primed ? this.resumeThreshold : this.initialThreshold;
        if (this.available < threshold) continue;
        this.playing = true;
        this.primed = true;
        this.fade = 0;
      }
      if (this.available === 0) {
        this.playing = false;
        this.tailGain = this.fade;
        this.underruns++;
        out[i] = this.lastSample * this.tailGain;
        this.tailGain = Math.max(0, this.tailGain - this.fadeStep);
        continue;
      }
      const readPos = (this.writePos - this.available + this.ring.length) % this.ring.length;
      this.fade = Math.min(1, this.fade + this.fadeStep);
      this.lastSample = this.ring[readPos];
      out[i] = this.lastSample * this.fade;
      this.available--;
    }
    this.reportCountdown -= out.length;
    if (this.reportCountdown <= 0) {
      this.reportCountdown = Math.floor(sampleRate / 4);
      this.port.postMessage({
        type: 'status',
        available: this.available,
        underruns: this.underruns,
      });
    }
    return true;
  }
}

registerProcessor('sdr-audio-processor', SdrAudioProcessor);
