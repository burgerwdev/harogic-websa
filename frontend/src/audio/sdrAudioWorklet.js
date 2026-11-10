/*
 * One backend ingress worker feeds this playback processor through its MessagePort.
 * The ring stores interleaved float values; stereo frames occupy two adjacent values.
 * Buffer thresholds are specified in seconds and scaled by the active channel count.
 * Fill-based drift correction advances the read position on the AudioContext clock.
 * Queue overflow clears playback and reports an explicit consumer failure.
 */
// Codec bursts use a larger ceiling than analog PCM.
// Retunes keep the normal prime: equal-rate playback cannot rebuild a short
// startup reserve, leaving later ingress jitter to cause an underrun and re-prime.
const PRIME_S = 0.45;
const TARGET_S = 0.50;
const MAX_FILL_S = 1.1;
const DRM_PRIME_S = 1.6;
const DRM_TARGET_S = 1.4;
const DRM_MAX_FILL_S = 2.4;
const FADE_S = 0.01;            // fade in/out, so a start or an underrun is not a click
const METER_S = 0.05;           // meter follows rendered PCM, independently of ingress buffering
const REPORT_S = 0.25;          // diagnostics window
const RATIO_MIN = 0.9;          // the drift corrector's range (±10% of the sink's clock)
const RATIO_MAX = 1.1;
// Fill averaging window, in seconds.
const FILL_WINDOW_S = 10;
// Fill error in seconds that requests the maximum correction step.
const FILL_ERROR_FOR_FULL_STEP = 0.05;
// Maximum ratio change per fill averaging window.
const DRIFT_STEP = 0.0005;
class SdrAudioProcessor extends AudioWorkletProcessor {
  // The data port is transferred to the ingress worker. Keep a separate,
  // audio-thread control for permanently retiring a source node: disconnecting
  // a source whose process() always returns true does not end its lifetime.
  static get parameterDescriptors() {
    return [{ name: 'active', defaultValue: 1, minValue: 0, maxValue: 1, automationRate: 'k-rate' }];
  }
  constructor() {
    super();
    this.capacity = Math.ceil(sampleRate * (DRM_MAX_FILL_S + 0.1)) * 2;
    // Capacity is float values: 2.5 seconds of stereo at the context sample rate.
    this.ring = new Float32Array(this.capacity);
    this.writePos = 0;
    this.readPos = 0;
    this.available = 0;
    this.target = Math.floor(sampleRate * TARGET_S);
    this.prime = Math.floor(sampleRate * PRIME_S);
    this.maxFill = Math.floor(sampleRate * MAX_FILL_S);
    this.mode = 'default';
    /// Sample layout of the ring: 'mono' (one value per sample) or 'stereo' (an L/R pair per
    /// sample, interleaved). The producer announces it with `buffer-mode`; a layout switch resets
    /// the ring, because mixing the two reads garbage.
    this.layout = 'mono';
    this.enabled = false;
    this.playing = false;
    this.primed = false;
    this.fade = 0;
    this.lastSample = 0;
    this.lastSampleR = 0;
    this.tailGain = 0;
    this.fadeStep = 1 / Math.max(1, sampleRate * FADE_S);
    // Drift correction: input samples consumed per output sample, steered by the fill.
    this.ratio = 1;
    this.steerCountdown = Math.floor(sampleRate * FILL_WINDOW_S);
    this.frac = 0;
    this.received = 0;
    this.renderedFrames = 0;
    this.playedFrames = 0;
    this.starvedFrames = 0;
    this.underruns = 0;
    /// Per-window fill accumulation (the trend is taken from the means, not from point samples).
    this.fillSum = 0;
    this.fillCount = 0;
    this.resets = 0;
    this.slipped = 0;
    this.reportCountdown = 0;
    this.meterFrames = 0;
    this.meterPower = 0;
    this.meterPeak = 0;
    this.failed = false;
    this.generation = 0;
    this.feedback = [];
    this.acceptedDelivery = null;
    // Source frames actually removed from the ring in the current generation.
    this.consumedFrames = 0;
    this.port.onmessage = (event) => {
      const message = event.data || {};
      if (message.type === 'samples' && message.samples) {
        this.push(message.samples);
        // This FIFO port admits deliveries in order. One cumulative watermark
        // at the meter cadence replaces a separate wakeup for every PCM block.
        if (!this.failed && message.generation === this.generation && Number.isSafeInteger(message.delivery)) this.acceptedDelivery = message.delivery;
      } else if (message.type === 'reset') {
        this.generation = message.generation ?? this.generation;
        this.setBufferMode(message.playbackMode);
        this.layout = message.stereo ? 'stereo' : 'mono';
        this.consumedFrames = 0;
        this.reset();
        this.postCredit();
      } else if (message.type === 'buffer-mode') {
        this.setBufferMode(message.mode);
        this.setLayout(message.stereo ? 'stereo' : 'mono');
        this.postCredit();
      } else if (message.type === 'enabled') {
        const wasPlaying = this.playing;
        this.enabled = Boolean(message.value);
        if (this.enabled) this.failed = false;
        if (!this.enabled) {
          this.tailGain = Math.max(this.tailGain, this.fade);
          this.available = 0;
          this.writePos = 0;
          this.readPos = 0;
          this.frac = 0;
          this.playing = false;
          this.primed = false;
          this.applyBufferParams();
          this.fade = 0;
          this.underruns = 0;
          if (wasPlaying) this.postGate();
        }
      }
    };
    this.port.postMessage({ type: 'ready', sampleRate });
  }

  /** Drop everything buffered: the stream was retuned, and the old audio must not be played. */
  reset() {
    const wasPlaying = this.playing;
    this.resets++;
    this.feedback.length = 0;
    this.acceptedDelivery = null;
    this.meterFrames = this.meterPower = this.meterPeak = 0;
    this.tailGain = 0;
    this.lastSample = 0;
    this.lastSampleR = 0;
    this.ratio = 1;
    this.fillSum = 0;
    this.fillCount = 0;
    this.steerCountdown = Math.floor(sampleRate * FILL_WINDOW_S);
    this.writePos = 0;
    this.readPos = 0;
    this.available = 0;
    this.frac = 0;
    this.playing = false;
    this.primed = false;
    this.applyBufferParams();
    this.fade = 0;
    this.underruns = 0;
    if (wasPlaying) this.postGate();
  }

  /** Playback transitions are time-critical bench data; the 250 ms status window is not. */
  postGate() {
    this.port.postMessage({
      type: 'gate-transition', playing: this.playing, primed: this.primed,
      generation: this.generation,
      available: this.available, resets: this.resets, underruns: this.underruns,
    });
  }

  /** Absolute source-frame allowance; repeated reports do not grant extra space. */
  postCredit(deferred = false) {
    const channels = this.layout === 'stereo' ? 2 : 1;
    const message = {
      type: 'credit', generation: this.generation,
      limit: this.maxFill / channels + this.consumedFrames,
    };
    if (deferred) this.feedback.push(message); else this.port.postMessage(message);
  }

  setBufferMode(mode) {
    const next = mode === 'drm' || mode === 'drmplus' ? 'drm' : 'default';
    if (next === this.mode) return;
    this.mode = next;
    // Frame counts in *samples per channel*; the layout multiplies them.
    this.primeFrames = Math.floor(sampleRate * (next === 'drm' ? DRM_PRIME_S : PRIME_S));
    this.targetFrames = Math.floor(sampleRate * (next === 'drm' ? DRM_TARGET_S : TARGET_S));
    this.maxFillFrames = Math.floor(sampleRate * (next === 'drm' ? DRM_MAX_FILL_S : MAX_FILL_S));
    this.applyBufferParams();
    this.ratio = 1;
  }

  /** Switch the ring's sample layout; the buffered audio of the old layout is dropped. */
  setLayout(layout) {
    const next = layout === 'stereo' ? 'stereo' : 'mono';
    if (next === this.layout) return;
    this.layout = next;
    this.applyBufferParams();
    this.reset();
  }

  /** The thresholds count values in the ring: a stereo pair is two values per frame. */
  applyBufferParams() {
    const mul = this.layout === 'stereo' ? 2 : 1;
    this.prime = (this.primeFrames || Math.floor(sampleRate * PRIME_S)) * mul;
    this.target = (this.targetFrames || Math.floor(sampleRate * TARGET_S)) * mul;
    this.maxFill = (this.maxFillFrames || Math.floor(sampleRate * MAX_FILL_S)) * mul;
  }

  push(samples) {
    if (this.failed || !this.enabled) return;
    const step = this.layout === 'stereo' ? 2 : 1;
    if (!(samples instanceof Float32Array) || samples.length % step !== 0
      || this.available + samples.length > Math.min(this.capacity, this.maxFill)) {
      this.failed = true;
      this.enabled = false;
      this.reset();
      this.port.postMessage({ type: 'error', message: 'Audio consumer is too slow or received invalid PCM: playback queue limit exceeded' });
      return;
    }
    const first = Math.min(samples.length, this.capacity - this.writePos);
    this.ring.set(samples.subarray(0, first), this.writePos);
    if (first < samples.length) this.ring.set(samples.subarray(first), 0);
    this.writePos = (this.writePos + samples.length) % this.capacity;
    this.available += samples.length;
    this.received += samples.length;
  }

  process(_inputs, outputs, parameters) {
    if (parameters?.active?.[0] === 0) return false;
    const out = outputs[0] && outputs[0][0];
    if (!out) return true;
    if (this.enabled) this.renderedFrames += out.length;
    const wasPlaying = this.playing;
    // The second channel is written whenever the node has one, in BOTH layouts: the ring carries
    // an L/R pair per sample in the stereo layout, and in the mono layout the same sample goes to
    // both. Writing the right channel only for the stereo layout left every mono mode — and a
    // mono FM station, which uses the stereo layout with an empty sideband — silent on the right
    // speaker ("only one speaker plays").
    const outR = outputs[0][1] || null;
    out.fill(0);
    if (outR) outR.fill(0);
    const stereo = this.layout === 'stereo';
    const step = stereo ? 2 : 1;                     // ring values consumed per output sample
    // The common, fully primed block has no gate/fade transitions. Keep ring
    // state in locals and wrap with a comparison, instead of modulo and many
    // object writes for every sample of every receiver. The slow path below
    // still owns priming, fading, starvation and their exact counters.
    const steady = this.enabled && this.playing && this.tailGain === 0 && this.fade === 1
      && this.available >= (Math.ceil(this.frac + out.length * this.ratio) + 2) * step;
    if (steady) {
      const ring = this.ring, capacity = this.capacity, ratio = this.ratio;
      let read = this.readPos, frac = this.frac, consumed = 0, value = 0, right = 0;
      for (let i = 0; i < out.length; i++) {
        let next = read + step; if (next >= capacity) next -= capacity;
        value = ring[read] * (1 - frac) + ring[next] * frac;
        right = stereo ? ring[read + 1] * (1 - frac) + ring[next + 1] * frac : value;
        out[i] = value; if (outR) outR[i] = right;
        frac += ratio;
        while (frac >= 1) {
          frac -= 1; read += step; if (read >= capacity) read -= capacity; consumed++;
        }
      }
      this.readPos = read; this.frac = frac; this.available -= consumed * step;
      this.consumedFrames += consumed; this.playedFrames += out.length;
      this.lastSample = value; this.lastSampleR = right;
    }
    for (let i = 0; !steady && i < out.length; i++) {
      if (this.tailGain > 0) {
        out[i] = this.lastSample * this.tailGain;
        if (outR) outR[i] = this.lastSampleR * this.tailGain;
        this.tailGain = Math.max(0, this.tailGain - this.fadeStep);
        if (this.enabled) this.starvedFrames++;
        continue;
      }
      if (!this.enabled) continue;
      if (!this.playing) {
        if (this.available < this.prime) {
          this.starvedFrames++;
          continue;
        }
        this.playing = true;
        this.primed = true;
        this.fade = 0;
      }
      if (this.available < 2 * step) {               // two frames for interpolation
        this.playing = false;
        this.tailGain = this.fade;
        this.underruns++;
        this.starvedFrames++;
        out[i] = this.lastSample * this.tailGain;
        if (outR) outR[i] = this.lastSampleR * this.tailGain;
        this.tailGain = Math.max(0, this.tailGain - this.fadeStep);
        continue;
      }
      const next = (this.readPos + step) % this.capacity;
      const value = this.ring[this.readPos] * (1 - this.frac) + this.ring[next] * this.frac;
      // In the mono layout the same sample feeds both channels.
      let valueR = value;
      if (stereo) {
        valueR = this.ring[this.readPos + 1] * (1 - this.frac)
          + this.ring[(next + 1) % this.capacity] * this.frac;
      }
      this.fade = Math.min(1, this.fade + this.fadeStep);
      this.lastSample = value;
      this.lastSampleR = valueR;
      out[i] = value * this.fade;
      if (outR) outR[i] = valueR * this.fade;
      this.playedFrames++;
      this.frac += this.ratio;
      while (this.frac >= 1) {
        this.frac -= 1;
        this.readPos = (this.readPos + step) % this.capacity;
        this.available -= step;
        this.consumedFrames++;
      }
    }
    // Measure the actual rendered block (including fades and silence). Stereo
    // RMS averages both channels; sample peak uses the louder channel.
    for (let i = 0; i < out.length; i++) {
      const left = out[i], right = outR ? outR[i] : left;
      this.meterPower += (left * left + right * right) / 2;
      this.meterPeak = Math.max(this.meterPeak, Math.abs(left), Math.abs(right));
    }
    this.meterFrames += out.length;
    const feedbackDue = this.meterFrames >= sampleRate * METER_S;
    if (feedbackDue) {
      this.feedback.push({ type: 'meter', generation: this.generation, playing: this.playing,
        rms: Math.sqrt(this.meterPower / this.meterFrames), peak: this.meterPeak });
      this.meterFrames = this.meterPower = this.meterPeak = 0;
    }
    this.fillSum += this.available;
    if (this.playing !== wasPlaying) this.postGate();
    this.fillCount += 1;
    this.steerCountdown -= out.length;
    if (this.steerCountdown <= 0) {
      this.steerCountdown = Math.floor(sampleRate * FILL_WINDOW_S);
      this.steerRatio();
    }
    this.reportCountdown -= out.length;
    if (this.reportCountdown <= 0) {
      this.reportCountdown = Math.floor(sampleRate * REPORT_S);
      this.postCredit(true);
      this.feedback.push({
        type: 'status',
        generation: this.generation,
        sampleRate,
        bufferMode: this.mode,
        primeFrames: this.prime / (this.layout === 'stereo' ? 2 : 1),
        maxFillFrames: this.maxFill / (this.layout === 'stereo' ? 2 : 1),
        available: this.available,
        underruns: this.underruns,
        received: this.received,
        renderedFrames: this.renderedFrames,
        playedFrames: this.playedFrames,
        starvedFrames: this.starvedFrames,
        resets: this.resets,
        slipped: this.slipped,
        ratio: this.ratio,
        playing: this.playing,
        primed: this.primed,
      });
    }
    if (feedbackDue) {
      if (this.acceptedDelivery !== null) {
        this.feedback.push({ type: 'accepted-through', delivery: this.acceptedDelivery, generation: this.generation });
        this.acceptedDelivery = null;
      }
      this.port.postMessage({ type: 'feedback', messages: this.feedback });
      this.feedback = [];
    }
    return true;
  }

  /**
   * Steer from window-mean fill error rather than individual producer bursts.
   * Apply one proportional correction per window, clamped to under a cent.
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
    const channels = this.layout === 'stereo' ? 2 : 1;
    const errorSeconds = (mean - this.target) / (sampleRate * channels);
    const step = Math.max(-DRIFT_STEP, Math.min(DRIFT_STEP,
      (errorSeconds / FILL_ERROR_FOR_FULL_STEP) * DRIFT_STEP));
    if (step !== 0) {
      this.ratio = Math.min(RATIO_MAX, Math.max(RATIO_MIN, this.ratio + step));
    }
  }
}

registerProcessor('sdr-audio-processor', SdrAudioProcessor);
