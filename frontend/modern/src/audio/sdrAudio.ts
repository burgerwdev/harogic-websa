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
        const idx = (writePos - available + ring.length) % ring.length;
        out[i] = ring[idx];
        available--;
      } else {
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
    try {
      ensureContext();
      void ctx?.resume();
    } catch {
      /* no audio device (e.g. headless): ignore */
    }
  }
  if (!on) {
    available = 0;
  }
}

export function isSdrAudioEnabled(): boolean {
  return enabled;
}

export function setSdrAudioRate(rate: number): void {
  if (rate > 0) sourceRate = rate;
}

export function pushSdrAudio(buffer: ArrayBuffer, offset: number, samples: number, rate: number): void {
  if (!enabled) return;
  try {
    ensureContext();
  } catch {
    return;                       // no audio device: keep the data path harmless
  }
  if (rate > 0) sourceRate = rate;
  if (samples * 2 + offset > buffer.byteLength) return;
  const pcm = new Int16Array(buffer, offset, samples);
  // If the context runs at a different rate, do a cheap linear resample ratio.
  const ratio = ctx ? (ctx.sampleRate / sourceRate) : 1;
  for (let i = 0; i < samples; i++) {
    const value = pcm[i] / 32768;
    if (Math.abs(ratio - 1) < 1e-4) {
      ring[writePos] = value;
      writePos = (writePos + 1) % ring.length;
      if (available < ring.length) available++;
    } else {
      // simple hold: not sample-accurate but keeps timing close
      const reps = Math.max(1, Math.round(ratio));
      for (let r = 0; r < reps; r++) {
        ring[writePos] = value;
        writePos = (writePos + 1) % ring.length;
        if (available < ring.length) available++;
      }
    }
  }
}

export function sdrAudioBufferedMs(): number {
  return (available / sourceRate) * 1000;
}
