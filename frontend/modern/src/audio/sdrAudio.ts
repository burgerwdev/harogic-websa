// SDR audio playback. AudioWorklet owns the real-time ring buffer; older/non-secure
// browsers fall back to ScriptProcessor so LAN deployments over HTTP still have audio.
import { StreamingPcm16Resampler } from './sdrResampler';

let ctx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let legacyNode: ScriptProcessorNode | null = null;
let legacyOscillator: OscillatorNode | null = null;
let legacyPullGain: GainNode | null = null;
let initPromise: Promise<void> | null = null;
let enabled = false;
let sourceRate = 48000;
let bufferedSamples = 0;
let audioUnderruns = 0;
let pendingChunks: Float32Array[] = [];
let resumeListenersInstalled = false;

// Legacy-only ring state.
const legacyRing = new Float32Array(48000 * 2);
let legacyWritePos = 0;
let legacyAvailable = 0;
let legacyFade = 0;
let legacyLastSample = 0;
let legacyTailGain = 0;
let legacyWasEmpty = true;

// Streaming linear resampler state is carried across WebSocket frames.
const resampler = new StreamingPcm16Resampler();

const PENDING_LIMIT = 100;
const FADE_STEP = 0.002;
const WORKLET_URL = new URL('./sdrAudioWorklet.js', import.meta.url).href;

export function initSdrAudio(): void {
  // AudioContext creation is deferred until a user enables audio.
}

function createContext(): AudioContext {
  try {
    return new AudioContext({ sampleRate: sourceRate });
  } catch {
    return new AudioContext();
  }
}

function setupLegacyNode(context: AudioContext): void {
  if (legacyNode) return;
  legacyNode = context.createScriptProcessor(2048, 0, 1);
  legacyNode.onaudioprocess = (event: AudioProcessingEvent) => {
    const out = event.outputBuffer.getChannelData(0);
    for (let i = 0; i < out.length; i++) {
      if (legacyTailGain > 0) {
        out[i] = legacyLastSample * legacyTailGain;
        legacyTailGain = Math.max(0, legacyTailGain - FADE_STEP);
        if (legacyTailGain === 0) {
          legacyWasEmpty = true;
          legacyFade = 0;
        }
      } else if (enabled && legacyAvailable > 0) {
        if (legacyWasEmpty) { legacyFade = 0; legacyWasEmpty = false; }
        const readPos = (legacyWritePos - legacyAvailable + legacyRing.length) % legacyRing.length;
        legacyFade = Math.min(1, legacyFade + FADE_STEP);
        legacyLastSample = legacyRing[readPos];
        out[i] = legacyLastSample * legacyFade;
        legacyAvailable--;
        if (legacyAvailable === 0) {
          legacyTailGain = legacyFade;
          audioUnderruns++;
        }
      } else {
        legacyWasEmpty = true;
        legacyFade = 0;
        out[i] = 0;
      }
    }
    bufferedSamples = legacyAvailable;
  };
  legacyOscillator = context.createOscillator();
  legacyPullGain = context.createGain();
  legacyPullGain.gain.value = 0;
  legacyOscillator.connect(legacyPullGain);
  legacyPullGain.connect(legacyNode);
  legacyOscillator.start();
  legacyNode.connect(context.destination);
}

function writeLegacy(samples: Float32Array): void {
  for (let i = 0; i < samples.length; i++) {
    legacyRing[legacyWritePos] = samples[i];
    legacyWritePos = (legacyWritePos + 1) % legacyRing.length;
    if (legacyAvailable < legacyRing.length) legacyAvailable++;
  }
  bufferedSamples = legacyAvailable;
}

function deliver(samples: Float32Array): void {
  if (workletNode) {
    // MessagePort preserves reset/sample ordering; this fresh buffer is safe to transfer.
    workletNode.port.postMessage({ type: 'samples', samples }, [samples.buffer]);
  } else if (legacyNode) {
    writeLegacy(samples);
  } else {
    pendingChunks.push(samples);
    if (pendingChunks.length > PENDING_LIMIT) pendingChunks.shift();
  }
}

function flushPending(): void {
  const chunks = pendingChunks;
  pendingChunks = [];
  for (const chunk of chunks) deliver(chunk);
}

async function initializeOutput(context: AudioContext): Promise<void> {
  let candidate: AudioWorkletNode | null = null;
  if (context.audioWorklet && typeof AudioWorkletNode !== 'undefined') {
    try {
      await context.audioWorklet.addModule(WORKLET_URL);
      candidate = new AudioWorkletNode(context, 'sdr-audio-processor', {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        outputChannelCount: [1],
      });
      await new Promise<void>((resolve, reject) => {
        const timeout = window.setTimeout(
          () => reject(new Error('AudioWorklet processor ready timeout')), 1000);
        candidate!.onprocessorerror = () => {
          window.clearTimeout(timeout);
          reject(new Error('AudioWorklet processor failed during startup'));
        };
        candidate!.port.onmessage = (event: MessageEvent) => {
          if (event.data?.type === 'ready') {
            window.clearTimeout(timeout);
            resolve();
          } else if (event.data?.type === 'status') {
            bufferedSamples = Number(event.data.available) || 0;
            audioUnderruns = Number(event.data.underruns) || 0;
          }
        };
        candidate!.connect(context.destination);
      });
      workletNode = candidate;
      candidate.onprocessorerror = () => {
        console.warn('SDR AudioWorklet processor stopped; using compatibility output');
        candidate?.disconnect();
        candidate?.port.close();
        if (workletNode === candidate) workletNode = null;
        setupLegacyNode(context);
        flushPending();
      };
      workletNode.port.postMessage({ type: 'enabled', value: enabled });
      flushPending();
      return;
    } catch (error) {
      candidate?.disconnect();
      candidate?.port.close();
      workletNode = null;
      console.warn('AudioWorklet unavailable; using compatibility audio output', error);
    }
  }
  setupLegacyNode(context);
  flushPending();
}

function installResumeListeners(): void {
  if (resumeListenersInstalled) return;
  resumeListenersInstalled = true;
  const resume = () => {
    if (!enabled || !ctx) return;
    void ctx.resume().then(() => {
      if (ctx?.state !== 'running') return;
      document.removeEventListener('click', resume);
      document.removeEventListener('keydown', resume);
      resumeListenersInstalled = false;
    });
  };
  document.addEventListener('click', resume);
  document.addEventListener('keydown', resume);
}

function ensureContext(): void {
  if (!ctx) {
    ctx = createContext();
    installResumeListeners();
  }
  if (!initPromise) {
    initPromise = initializeOutput(ctx).catch((error) => {
      console.warn('SDR audio output initialization failed', error);
    });
  }
  if (enabled) {
    if (ctx.state !== 'running') installResumeListeners();
    void ctx.resume();
  }
}

function resetPlayback(): void {
  pendingChunks = [];
  bufferedSamples = 0;
  audioUnderruns = 0;
  legacyAvailable = 0;
  legacyTailGain = legacyFade;
  legacyFade = 0;
  legacyWasEmpty = true;
  resampler.reset();
  workletNode?.port.postMessage({ type: 'reset' });
}

export function setSdrAudioEnabled(on: boolean): void {
  enabled = on;
  if (on) {
    try {
      ensureContext();
      workletNode?.port.postMessage({ type: 'enabled', value: true });
      void ctx?.resume();
    } catch {
      // Headless browsers and hosts without an audio device keep the data path harmless.
    }
  } else {
    resetPlayback();
    workletNode?.port.postMessage({ type: 'enabled', value: false });
  }
}

export function isSdrAudioEnabled(): boolean {
  return enabled;
}

export function setSdrAudioRate(rate: number): void {
  if (rate > 0) sourceRate = rate;
}

function resamplePcm(buffer: ArrayBuffer, offset: number, samples: number): Float32Array {
  return resampler.process(buffer, offset, samples, sourceRate, ctx?.sampleRate || sourceRate);
}

export function pushSdrAudio(
  buffer: ArrayBuffer, offset: number, samples: number, rate: number, reset = false,
): void {
  if (!enabled) return;
  if (samples * 2 + offset > buffer.byteLength) return;
  if (reset) resetPlayback();
  if (samples === 0) return;
  if (rate > 0) sourceRate = rate;
  try {
    ensureContext();
  } catch {
    return;
  }
  const output = resamplePcm(buffer, offset, samples);
  if (output.length) deliver(output);
}

export function sdrAudioBufferedMs(): number {
  const rate = ctx?.sampleRate || sourceRate;
  return (bufferedSamples / rate) * 1000;
}

export function sdrAudioUnderrunCount(): number {
  return audioUnderruns;
}
