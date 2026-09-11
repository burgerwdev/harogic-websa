// SDR audio playback: a small ring buffer fed by AUDF WebSocket frames and drained
// by a ScriptProcessorNode. The AudioContext is created at 48 kHz to match the
// backend audio rate, and resumed on a user gesture (the SDR button click).
let ctx: AudioContext | null = null;
let node: ScriptProcessorNode | null = null;
let ring = new Float32Array(48000 * 2); // 2 s
let writePos = 0;
let available = 0;
let enabled = false;
let sourceRate = 48000;
let fade = 0;
let wasEmpty = true;

const FADE_STEP = 0.002;   // ~10 ms fade in/out at 48 kHz per sample-tick

export function initSdrAudio(): void {
  // nothing to set up until the user enables audio (needs a gesture)
}

function ensureContext(): void {
  if (ctx) return;
  try {
    ctx = new AudioContext({ sampleRate: sourceRate });
  } catch {
    ctx = new AudioContext();
  }
  node = ctx.createScriptProcessor(2048, 0, 1);
  node.onaudioprocess = (event: AudioProcessingEvent) => {
    const out = event.outputBuffer.getChannelData(0);
    for (let i = 0; i < out.length; i++) {
      if (available > 0) {
        if (wasEmpty) { fade = 0; wasEmpty = false; }   // fade in after any gap
        const idx = (writePos - available + ring.length) % ring.length;
        if (fade < 1) fade = Math.min(1, fade + FADE_STEP);
        out[i] = ring[idx] * fade;
        available--;
      } else {
        wasEmpty = true;
        if (fade > 0) fade = Math.max(0, fade - FADE_STEP);
        out[i] = 0;
      }
    }
  };
  // A silent oscillator keeps the processor pulling even with no other input.
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  gain.gain.value = 0;
  osc.connect(gain);
  gain.connect(node);
  osc.start();
  node.connect(ctx.destination);
  // If the context is created without a user gesture (page reload in SDR mode), a
  // later click/keypress should resume it.
  const resume = () => {
    void ctx?.resume();
    document.removeEventListener('click', resume);
    document.removeEventListener('keydown', resume);
  };
  document.addEventListener('click', resume);
  document.addEventListener('keydown', resume);
}

export function setSdrAudioEnabled(on: boolean): void {
  enabled = on;
  if (on) {
    fade = 0;                 // fade in to avoid a click
    try {
      ensureContext();
      void ctx?.resume();
    } catch {
      /* no audio device (e.g. headless): ignore */
    }
  } else {
    available = 0;
    writePos = 0;
    fade = 0;
    rsPrev = null;
    rsT = 0;
  }
}

export function isSdrAudioEnabled(): boolean {
  return enabled;
}

export function setSdrAudioRate(rate: number): void {
  if (rate > 0) sourceRate = rate;
}

// Streaming linear resampler phase (input samples), carried across blocks so the audio
// is sample-accurate even when the AudioContext rate differs from the backend rate.
let rsT = 0;
let rsPrev: number | null = null;

function writeRing(value: number): void {
  ring[writePos] = value;
  writePos = (writePos + 1) % ring.length;
  if (available < ring.length) available++;
}

export function pushSdrAudio(
  buffer: ArrayBuffer, offset: number, samples: number, rate: number, reset = false,
): void {
  if (!enabled) return;
  if (reset) {
    available = 0;
    writePos = 0;
    fade = 0;
    wasEmpty = true;
    rsPrev = null;
    rsT = 0;
  }
  if (samples === 0) return;
  try {
    ensureContext();
  } catch {
    return;                       // no audio device: keep the data path harmless
  }
  if (rate > 0) sourceRate = rate;
  if (samples * 2 + offset > buffer.byteLength) return;
  const pcm = new Int16Array(buffer, offset, samples);
  let buf = new Float32Array(samples);
  for (let i = 0; i < samples; i++) buf[i] = pcm[i] / 32768;
  const outRate = ctx ? ctx.sampleRate : sourceRate;
  const step = sourceRate / outRate;          // input samples per output sample
  if (rsPrev !== null) {
    const b = new Float32Array(samples + 1);
    b[0] = rsPrev; b.set(buf, 1); buf = b;
  }
  const L = buf.length;
  const n = Math.floor((L - 1 - rsT) / step) + 1;
  for (let k = 0; k < n; k++) {
    const pos = rsT + k * step;
    const i0 = Math.min(Math.floor(pos), L - 2);
    const fr = Math.min(1, Math.max(0, pos - i0));
    writeRing(buf[i0] * (1 - fr) + buf[i0 + 1] * fr);
  }
  rsT = (n > 0 ? rsT + (n - 1) * step : rsT) + step - (L - 1);
  rsPrev = buf[L - 1];
}

export function sdrAudioBufferedMs(): number {
  return (available / sourceRate) * 1000;
}
