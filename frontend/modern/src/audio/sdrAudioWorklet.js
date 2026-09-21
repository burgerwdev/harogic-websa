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
// The fill sizes: the *prime* is both the start threshold and (with a well-behaved producer) the
// steady state, because a matched producer leaves the fill wherever it started playing. The ceiling
// is the headroom a mismatch gets before it is corrected OR trimmed - measured jitter is a few
// milliseconds and the analyzer's stalls reach a few hundred, so priming at 250 ms rides them out.
const PRIME_S = 0.25;           // start playing once this much is buffered
const TARGET_S = 0.3;           // the fill the corrector aims at (and the trend's reference)
// Past this the oldest samples are dropped: a producer that outruns the clock must not become
// growing latency, and one window of an uncorrected mismatch (~3%) still fits.
const MAX_FILL_S = 0.9;
const FADE_S = 0.01;            // fade in/out, so a start or an underrun is not a click
const REPORT_S = 0.25;          // diagnostics window
const RATIO_MIN = 0.9;          // the drift corrector's range (±10% of the sink's clock)
const RATIO_MAX = 1.1;
/// How often the fill's window mean is compared against its band.
const FILL_WINDOW_S = 10;
/// Target fill (the ring's occupancy the listener is meant to hear through).
const FILL_TARGET_S = 0.3;
/// The proportional gain, expressed as "a fill error this large asks for a full step" (seconds): a
/// proportional loop parks the fill `offset / gain` from the target, and this keeps that within the
/// band for the residual this stage is sized for.
const FILL_ERROR_FOR_FULL_STEP = 0.05;
/// The largest correction one window may make: 0.05% ~ 0.9 cent, a step the ear does not hear.
///
/// The correction is a *pitch*, so this stage only ever touches it in steps this size and the step
/// shrinks as the fill approaches its target - when the loop has converged, the ratio stops changing
/// entirely, which is what the listener hears as a steady tone.
///
/// A fixed-size step instead (bang-bang) chatters around the band's edge: the step can be smaller than
/// the offset, so the fill sits on the edge and every window adds another step - the ratio then walks
/// past the offset, which is an audible few-cent wander. A proportional term sized for the real
/// residual (~0.05%) parks the fill a little off target (offset / gain) and leaves the pitch alone.
///
/// ponytail: that parking offset means a mismatch several times the design range pushes the fill to
/// the ceiling and its trim drops samples. That is the right trade here - the real residual measured
/// 0.05%, and a proportional term would only be worth it if that grew much larger.
const DRIFT_STEP = 0.0005;
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
    this.steerCountdown = Math.floor(sampleRate * FILL_WINDOW_S);
    this.frac = 0;
    this.received = 0;
    this.underruns = 0;
    /// Per-window fill accumulation (the trend is taken from the means, not from point samples).
    this.fillSum = 0;
    this.fillCount = 0;
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
    this.fillSum += this.available;
    this.fillCount += 1;
    this.steerCountdown -= out.length;
    if (this.steerCountdown <= 0) {
      this.steerCountdown = Math.floor(sampleRate * FILL_WINDOW_S);
      this.steerRatio();
    }
    this.reportCountdown -= out.length;
    if (this.reportCountdown <= 0) {
      this.reportCountdown = Math.floor(sampleRate * REPORT_S);
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
   * Steer the resampling ratio by the fill's window-mean error: one small, shrinking step per window.
   *
   * The fill jitters by hundreds of milliseconds as the producer's blocks land, so only the window
   * *mean* says anything about the rate. A controller that read the instantaneous fill - or that took
   * the difference of two point samples as the rate - moved the pitch on that jitter, and the listener
   * heard the audio speed up and slow down every few seconds. What is left after averaging is small
   * enough for one proportional step per window, clamped to under a cent.
   */
  steerRatio() {
    if (!this.primed) {
      this.fillSum = 0;
      this.fillCount = 0;
      return;
    }
    const mean = this.fillSum / Math.max(1, this.fillCount);
    this.fillSum = 0;
    this.fillCount = 0;
    const errorSeconds = mean / sampleRate - FILL_TARGET_S;
    const step = Math.max(-DRIFT_STEP, Math.min(DRIFT_STEP,
      (errorSeconds / FILL_ERROR_FOR_FULL_STEP) * DRIFT_STEP));
    if (step !== 0) {
      this.ratio = Math.min(RATIO_MAX, Math.max(RATIO_MIN, this.ratio + step));
    }
  }
}

registerProcessor('sdr-audio-processor', SdrAudioProcessor);
