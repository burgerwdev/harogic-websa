/*
 * SDR playback processor: the last stage between the demodulator and the speaker.
 *
 * The producer is not the audio clock: PCM arrives from a worker in ~20 ms blocks, paced by the
 * analyzer's IQ packets, while this processor is called on the sound card's clock. The two differ
 * by a small fraction of a percent that nobody controls (a vendor's nominal sample rate, the
 * device's own clock), and a jittery network/worker adds bursts on top. A bare ring buffer cannot
 * absorb either: a rate mismatch slowly drains or fills it, and every empty moment is an audible
 * fade-out/re-prime (measured before this: about one puff per second, and a fill that grew until
 * the writer lapped the reader).
 *
 * So this stage does three things:
 *   * a jitter buffer with a target fill (~250 ms) that tolerates burst arrival;
 *   * a drift-correcting resampler: the read pointer advances by a ratio steered by the fill, so a
 *     persistent producer/consumer mismatch is absorbed by resampling (which also keeps the pitch
 *     right) instead of by draining the buffer;
 *   * a hard ceiling on the fill, so a producer that outruns the clock can never turn into growing
 *     latency: past `MAX_FILL` the oldest samples are dropped (the audio stays live, which is what
 *     a listener wants after a retune).
 */
const TARGET_S = 0.25;          // steady-state fill: burst tolerance
const PRIME_S = 0.12;           // start playing as soon as this much is buffered
const MAX_FILL_S = 0.5;         // never hold more than this (a live edge beats low latency here)
const FADE_S = 0.01;            // fade in/out, so a start or an underrun is not a click
const REPORT_S = 0.25;          // diagnostics window
const RATIO_MIN = 0.9;          // the drift corrector's range (±10% of the sink's clock)
const RATIO_MAX = 1.1;
const RATIO_GAIN = 0.25;        // how hard the fill error steers the ratio, per report window

class SdrAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.capacity = Math.ceil(sampleRate * (MAX_FILL_S + 0.1));
    this.ring = new Float32Array(this.capacity);
    this.writePos = 0;
    this.readPos = 0;
    this.available = 0;
    this.target = Math.floor(sampleRate * TARGET_S);
    this.prime = Math.floor(sampleRate * PRIME_S);
    this.maxFill = Math.floor(sampleRate * MAX_FILL_S);
    this.enabled = false;
    this.playing = false;
    this.primed = false;
    this.fade = 0;
    this.lastSample = 0;
    this.tailGain = 0;
    this.fadeStep = 1 / Math.max(1, sampleRate * FADE_S);
    // Drift correction: input samples consumed per output sample, steered by the fill.
    this.ratio = 1;
    this.frac = 0;
    this.received = 0;
    this.underruns = 0;
    this.resets = 0;
    this.slipped = 0;
    this.reportCountdown = 0;
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
          this.writePos = 0;
          this.readPos = 0;
          this.frac = 0;
          this.playing = false;
          this.primed = false;
          this.fade = 0;
          this.underruns = 0;
        }
      }
    };
    this.port.postMessage({ type: 'ready' });
  }

  /** Drop everything buffered: the stream was retuned, and the old audio must not be played. */
  reset() {
    this.resets++;
    this.tailGain = this.fade;
    this.writePos = 0;
    this.readPos = 0;
    this.available = 0;
    this.frac = 0;
    this.playing = false;
    this.primed = false;
    this.fade = 0;
    this.underruns = 0;
  }

  push(samples) {
    for (let i = 0; i < samples.length; i++) {
      this.ring[this.writePos] = samples[i];
      this.writePos = (this.writePos + 1) % this.capacity;
      if (this.available < this.capacity) {
        this.available++;
      } else {
        // The ring is full: this sample replaces the oldest unplayed one. Dropping it (rather than
        // growing the backlog) is what keeps a producer that outruns the clock from turning into
        // latency - the listener hears live audio with an occasional skip instead of the past.
        this.readPos = (this.readPos + 1) % this.capacity;
        this.slipped++;
      }
      this.received++;
    }
    // ...and never hold more than the ceiling, so the latency stays bounded.
    const excess = this.available - this.maxFill;
    if (excess > 0) {
      this.readPos = (this.readPos + excess) % this.capacity;
      this.available -= excess;
      this.slipped += excess;
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
        if (this.available < this.prime) continue;    // still priming: output silence
        this.playing = true;
        this.primed = true;
        this.fade = 0;
      }
      if (this.available < 2) {                        // nothing to interpolate between
        this.playing = false;
        this.tailGain = this.fade;
        this.underruns++;
        out[i] = this.lastSample * this.tailGain;
        this.tailGain = Math.max(0, this.tailGain - this.fadeStep);
        continue;
      }
      const next = (this.readPos + 1) % this.capacity;
      const value = this.ring[this.readPos] * (1 - this.frac) + this.ring[next] * this.frac;
      this.fade = Math.min(1, this.fade + this.fadeStep);
      this.lastSample = value;
      out[i] = value * this.fade;
      this.frac += this.ratio;
      while (this.frac >= 1) {
        this.frac -= 1;
        this.readPos = (this.readPos + 1) % this.capacity;
        this.available--;
      }
    }
    this.reportCountdown -= out.length;
    if (this.reportCountdown <= 0) {
      this.reportCountdown = Math.floor(sampleRate * REPORT_S);
      this.steerRatio();
      this.port.postMessage({
        type: 'status',
        available: this.available,
        underruns: this.underruns,
        received: this.received,
        resets: this.resets,
        slipped: this.slipped,
        ratio: this.ratio,
      });
    }
    return true;
  }

  /**
   * Steer the resampling ratio from the fill error.
   *
   * Above the target the producer is ahead (or was bursty): consume faster. Below it, slower. The
   * loop settles on the true producer/sink rate ratio, which is what keeps the fill (and the
   * latency) bounded without dropping or repeating samples.
   */
  steerRatio() {
    if (!this.primed) return;
    const error = (this.available - this.target) / Math.max(1, this.target);
    const next = 1 + RATIO_GAIN * error;
    this.ratio = Math.min(RATIO_MAX, Math.max(RATIO_MIN, next));
  }
}

registerProcessor('sdr-audio-processor', SdrAudioProcessor);
