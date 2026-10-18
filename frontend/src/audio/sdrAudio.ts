// SDR audio playback.
//
// Two producers can feed the worklet, and exactly one of them runs:
//
//   * the browser DSP (`sdr/iqStream.ts`): it is handed the worklet's port once its pipeline is
//     producing PCM, and this module then starts no Python audio path at all - no `?audio=1`
//     socket, no resampling worker (measured: running both meant the backend computed a second
//     demodulator chain and shipped audio the browser discarded);
//   * the Python reference (`?wasm=0`, a browser without WebAssembly, or a DSP failure): the
//     worker below owns the audio-only socket and drives the port, or posts the resampled blocks
//     back for the legacy ScriptProcessor output.
//
// The AudioContext and the AudioWorklet must live on the main thread (Web Audio is not
// available in workers), but the *ingress* does not: the worker owns the socket and drives the
// worklet's MessagePort directly (the port is transferred to it). Neither reception nor delivery
// therefore depends on the main thread, which is what used to make the ring underrun whenever
// rendering stalled.
//
// Browsers without AudioWorklet keep the ScriptProcessor output, which is main-thread only;
// there the worker posts the resampled buffers back and this module writes the legacy ring.
import { wasmDspAllowed } from '../sdr/capability';

let ctx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
/// A tap on the final audio node, for answering "what is actually reaching the speaker?" live.
let spectrumProbe: AnalyserNode | null = null;
let spectrumProbeBin = 1;
let worker: Worker | null = null;
//: True once the worklet's port has been handed to the WASM DSP worker. Its port can only be
//: transferred once (a second transfer throws "Port at index 0 is already neutered"), so both the
//: handoff and the Python audio worker's own transfer have to respect this.
let workletPortHandedOff = false;
//: True once the browser DSP failed for real and the Python audio path took playback over.
//: The policy answer (`wasmDspAllowed`) is about permission, not health: after a genuine DSP
//: failure the output belongs to Python for the rest of the session, whoever asks.
let pythonOwnsPlayback = false;
//: Ports that have already been transferred at least once. A MessagePort cannot be transferred
//: twice, and the two consumers (the Python audio worker and the WASM DSP worker) can both reach
//: the same port through different code paths; this makes the mistake impossible at the point of
//: transfer instead of relying on every caller to check a flag.
const transferredPorts = new WeakSet<MessagePort>();

/** Transfer `port` to `send` at most once; returns false when the port was already moved. */
function transferOnce(port: MessagePort, send: (port: MessagePort) => void): boolean {
  if (transferredPorts.has(port)) return false;
  try {
    send(port);
  } catch {
    return false;
  }
  transferredPorts.add(port);
  return true;
}
let legacyNode: ScriptProcessorNode | null = null;
let legacyOscillator: OscillatorNode | null = null;
let legacyPullGain: GainNode | null = null;
let initPromise: Promise<void> | null = null;
let enabled = false;
let audioTransitionMuted = false;
let audioFrames = 0;
let bufferedSamples = 0;
let audioUnderruns = 0;
let audioRms = 0;
let transitionWatchdog: number | null = null;
let resumeListenersInstalled = false;

// Legacy-only ring state.
const legacyRing = new Float32Array(48000 * 2);
let legacyWritePos = 0;
let legacyAvailable = 0;
let legacyFade = 0;
let legacyLastSample = 0;
let legacyTailGain = 0;
let legacyWasEmpty = true;

const FADE_STEP = 0.002;
const WORKLET_URL = new URL('./sdrAudioWorklet.js', import.meta.url).href;

function createContext(): AudioContext {
  try {
    return new AudioContext();
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

/** Audio-only WebSocket URL for the worker (the display connection carries no audio). */
function audioWorkerUrl(): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const token = sessionStorage.getItem('web-sa-token');
  const q = token ? `&token=${encodeURIComponent(token)}` : '';
  return `${protocol}//${location.host}/ws?audio=1${q}`;
}

/**
 * Start the audio ingress worker. With `port` it drives the worklet directly; without it
 * (legacy ScriptProcessor fallback) it posts the resampled buffers back to this thread.
 */
/** Average an interleaved L/R block into one mono channel (the legacy ring's layout). */
function mixdown(stereo: Float32Array): Float32Array {
  const out = new Float32Array(stereo.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = (stereo[2 * i] + stereo[2 * i + 1]) / 2;
  }
  return out;
}

function startWorker(port: MessagePort | null): void {
  if (worker) return;
  worker = new Worker(new URL('./sdrAudioWorker.ts', import.meta.url), { type: 'module' });
  worker.onmessage = (event: MessageEvent) => {
    const d = (event.data || {}) as Record<string, any>;
    if (d.type === 'samples' && d.samples) {
      // The legacy ring is mono by construction (ScriptProcessor, one channel): a stereo block
      // is mixed down, so the fallback path keeps working whatever the producer sends.
      const block = d.samples as Float32Array;
      writeLegacy(block.length % 2 === 0 && d.stereo ? mixdown(block) : block);
    } else if (d.type === 'reset') {
      legacyAvailable = 0;
      legacyTailGain = legacyFade;
      legacyFade = 0;
      legacyWasEmpty = true;
    } else if (d.type === 'stats') {
      if (typeof d.frames === 'number') audioFrames = d.frames;
      if (typeof d.rms === 'number') audioRms = d.rms;
      // The worklet path reports from the worklet, the legacy path from the ring.
      if (workletNode) {
        bufferedSamples = Number(d.bufferedSamples) || 0;
        audioUnderruns = Number(d.underruns) || 0;
      }
      publishAudioDebug();
    }
  };
  const init: Record<string, unknown> = {
    type: 'init',
    url: audioWorkerUrl(),
    targetRate: ctx?.sampleRate || 48000,
  };
  // Who owns playback: when the browser DSP is available (and has not failed) the IQ worker
  // claims the worklet port (`routeWorkletPortTo`), and a MessagePort can only be transferred
  // once - handing it to the Python audio worker here would leave the WASM PCM with nowhere to
  // go, which is exactly the reported "one blip, then silence" after switching the audio on:
  // the ring was fed by nobody. After a real DSP failure the ownership is Python's regardless
  // of what the policy says.
  const dspOwnsPlayback = wasmDspAllowed() && !pythonOwnsPlayback;
  if (port && !workletPortHandedOff && !dspOwnsPlayback) {
    init.port = port;
    transferOnce(port, (moved) => worker!.postMessage(init, [moved]));
  } else {
    // Without a port (legacy output), with the DSP owning it, or after it moved: the worker must
    // not be handed a neutered port, and the Python path keeps its socket either way.
    worker.postMessage(init);
  }
  worker.postMessage({ type: 'enabled', value: enabled });
  worker.postMessage({ type: 'mute', value: audioTransitionMuted });
}

async function initializeOutput(context: AudioContext): Promise<void> {
  let candidate: AudioWorkletNode | null = null;
  if (context.audioWorklet && typeof AudioWorkletNode !== 'undefined') {
    try {
      await context.audioWorklet.addModule(WORKLET_URL);
      candidate = new AudioWorkletNode(context, 'sdr-audio-processor', {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        // Two channels always: the WFM stereo layout plays L/R pairs, every other layout writes
        // silence into the second channel. A one-channel node would have nowhere to put the
        // right channel when stereo starts.
        outputChannelCount: [2],
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
          }
        };
        candidate!.connect(context.destination);
        // A spectrum probe on the *final* node, which is what the speaker gets: the PCM the DSP hands
        // over is not the whole story (the ring, its resampler and the fades come after it), so a
        // "periodic sound" that no amount of PCM analysis explains can be looked for here - and by
        // whoever is listening, from the console, while it is audible. `window.websaSpectrum()` returns
        // the strongest components (Hz, dB relative to the loudest) around the current playback.
        try {
          spectrumProbe?.disconnect();
          spectrumProbe = context.createAnalyser();
          spectrumProbe.fftSize = 16384;
          candidate!.connect(spectrumProbe);
          spectrumProbeBin = context.sampleRate / spectrumProbe.fftSize;
        } catch {
          spectrumProbe = null;
        }
      });
      workletNode = candidate;
      installWorkletFailureWatch(candidate, context);
      // The browser DSP owns playback when the policy allows it (and it has not failed):
      // then the Python audio path is not started (it is the fallback,
      // `enablePythonAudioFallback` starts it if the DSP fails).
      if (!wasmDspAllowed() || pythonOwnsPlayback) startWorker(candidate.port);
      return;
    } catch (error) {
      candidate?.disconnect();
      candidate?.port.close();
      workletNode = null;
      workletPortHandedOff = false;
      console.warn('AudioWorklet unavailable; using compatibility audio output', error);
    }
  }
  setupLegacyNode(context);
  if (!wasmDspAllowed() || pythonOwnsPlayback) startWorker(null);
}

/**
 * Start the Python audio path as the fallback.
 *
 * Called when the browser DSP reports that it cannot run (a module that failed to load, an ABI it
 * does not understand, a mode with no kernel). Returns false when there is nothing to fall back to
 * or when the path is already running, so a caller can report the difference.
 */
export function enablePythonAudioFallback(): boolean {
  if (worker || !enabled || !ctx) return false;
  pythonOwnsPlayback = true;
  if (workletNode && !workletPortHandedOff) {
    try {
      // `startWorker` already sends the current enabled/mute state to the new worker.
      startWorker(workletNode.port);
      return true;
    } catch {
      pythonOwnsPlayback = false;
      return false;
    }
  }
  // The worklet port was handed to the (now failed) browser DSP worker: a MessagePort cannot
  // move twice, so THAT output is unrecoverable. Build a fresh worklet output and give ITS
  // port to the Python worker; without AudioWorklet support the legacy ScriptProcessor
  // consumes the ring instead. Either way the Python PCM gets a speaker path.
  buildPythonOutput();
  return true;
}

/** Create a new worklet output owned by the Python audio worker (async, best effort). */
async function buildPythonOutput(): Promise<void> {
  const context = ctx;
  if (!context) return;
  let candidate: AudioWorkletNode | null = null;
  try {
    candidate = new AudioWorkletNode(context, 'sdr-audio-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
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
        }
      };
      candidate!.connect(context.destination);
    });
  } catch {
    candidate?.disconnect();
    candidate?.port.close();
    // The dead DSP-held worklet stays connected but silent; the legacy node plays the ring.
    setupLegacyNode(context);
    startWorker(null);
    return;
  }
  workletNode = candidate;
  installWorkletFailureWatch(candidate, context);
  // The spectrum probe taps the FINAL node; follow the output so console diagnostics keep
  // answering after the fallback rebuilt it.
  try {
    spectrumProbe?.disconnect();
    spectrumProbe = context.createAnalyser();
    spectrumProbe.fftSize = 16384;
    candidate.connect(spectrumProbe);
    spectrumProbeBin = context.sampleRate / spectrumProbe.fftSize;
  } catch {
    spectrumProbe = null;
  }
  startWorker(candidate.port);
}

/** The shared processorerror hand-off used by every worklet this module creates. */
function installWorkletFailureWatch(node: AudioWorkletNode, context: AudioContext): void {
  node.onprocessorerror = () => {
    console.warn('SDR AudioWorklet processor stopped; using compatibility output');
    node.disconnect();
    node.port.close();
    if (workletNode === node) workletNode = null;
    workletPortHandedOff = false;
    setupLegacyNode(context);
    worker?.postMessage({ type: 'detach' });
  };
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
  bufferedSamples = 0;
  audioUnderruns = 0;
  legacyAvailable = 0;
  legacyTailGain = legacyFade;
  legacyFade = 0;
  legacyWasEmpty = true;
  worker?.postMessage({ type: 'reset' });
}

function clearTransitionWatchdog(): void {
  if (transitionWatchdog !== null) {
    window.clearTimeout(transitionWatchdog);
    transitionWatchdog = null;
  }
}

export function prepareSdrAudioTransition(): void {
  if (!enabled) return;
  audioTransitionMuted = true;
  publishAudioDebug();
  resetPlayback();
  worker?.postMessage({ type: 'mute', value: true });
  // Safety net: a command that does not actually re-configure the chain never sends a
  // reset marker, so bound the mute instead of leaving the audio silent forever.
  clearTransitionWatchdog();
  transitionWatchdog = window.setTimeout(() => {
    transitionWatchdog = null;
    audioTransitionMuted = false;
    worker?.postMessage({ type: 'mute', value: false });
    publishAudioDebug();
  }, 700);
}

/**
 * Publish the audio gate state on the canvas so "the button says On but nothing is heard"
 * can be told apart from "no audio data arrived": enabled = we accept frames, muted = a
 * reconfiguration transition is swallowing them, frames = AUDF frames seen, buffered = the
 * worklet's ring. Read it in the browser console:
 *   document.getElementById('spectrum').dataset.sdrAudio
 */
function publishAudioDebug(): void {
  const cv = document.getElementById('spectrum');
  if (cv) {
    cv.dataset.sdrAudio =
      `enabled=${enabled} muted=${audioTransitionMuted} frames=${audioFrames}` +
      ` buffered_ms=${(bufferedSamples / 48).toFixed(1)} underruns=${audioUnderruns}` +
      ` rms=${audioRms.toFixed(4)}` +
      // ctx=running but buffered_ms climbing, or ctx=suspended, both explain "the button
      // says On but nothing is heard" without any state-management involvement.
      ` ctx=${ctx ? ctx.state : 'none'} worklet=${!!workletNode} worker=${!!worker}`;
  }
}

/**
 * Hand the worklet's MessagePort to another worker (the WASM DSP host).
 *
 * Returns false when there is no worklet to hand over (the legacy ScriptProcessor fallback): the
 * caller must then keep the Python audio path, so this answers instead of throwing. The Python
 * audio worker is detached first — two writers on one port would interleave two different sample
 * streams into the ring.
 */
/**
 * The output device's sample rate, i.e. what the DSP must produce.
 *
 * The worklet plays its input as-is, so a pipeline that always emitted 48 kHz would play at the
 * wrong pitch and speed whenever the device runs at 44.1 kHz.
 */
/**
 * The strongest components of what is actually being played right now (the final node, after every
 * stage), as `{ hz, db }` with the levels relative to the loudest one.
 *
 * Attached to `window.websaSpectrum` so it can be called from the console the moment a sound is
 * audible - a listener's report of "a periodic sound" that the PCM cannot explain (bit-identical to
 * the reference, and clean under an FFT) is either in this node's output or outside the browser.
 */
export function sampleSpectrum(
  top = 6,
): Array<{ hz: number; db: number; relativeDb: number; loudestDb: number }> {
  if (!spectrumProbe) return [];
  const bins = new Float32Array(spectrumProbe.frequencyBinCount);
  spectrumProbe.getFloatFrequencyData(bins);
  // The absolute level of the loudest bin is the difference between "this sound is here" and
  // "this node is silent" - a relative spectrum alone cannot tell them apart.
  const loudest = Math.max(...bins);
  const index: number[] = [];
  for (let i = 1; i < bins.length; i++) index.push(i);
  index.sort((a, b) => bins[b] - bins[a]);
  const out: Array<{ hz: number; db: number; relativeDb: number; loudestDb: number }> = [];
  const used: number[] = [];
  for (const i of index) {
    if (!Number.isFinite(bins[i])) continue;
    // One entry per peak: neighbouring bins of the same component are not separate findings.
    if (used.some((u) => Math.abs(u - i) < 4)) continue;
    used.push(i);
    out.push({
      hz: i * spectrumProbeBin,
      db: bins[i],
      relativeDb: bins[i] - bins[index[0]],
      loudestDb: loudest,
    });
    if (out.length >= top) break;
  }
  return out;
}

/**
 * The level of what is being played, in ~20 ms steps: the shape a "periodic sound" has in time.
 *
 * A spectrum cannot show it - the reported sound is a level that rises and falls, not a tone (the
 * final node's spectrum is a flat noise floor, measured). `min`/`max` say how deep it is and the
 * series says how fast, so a period can be read straight off it.
 *
 * Async and yield-between-reads: the diagnostic runs on the main thread, and the old busy loop
 * held it for the whole window while reporting thousands of duplicate readings of the same
 * buffer. One read per animation frame samples the live signal without blocking anything.
 */
export async function sampleEnvelope(ms = 1000): Promise<Record<string, number | number[]>> {
  const empty = { stepMs: 20, count: 0, min: 0, mean: 0, max: 0, ratio: 1, levels: [] as number[] };
  if (!spectrumProbe) return empty;
  const step = Math.round(spectrumProbe.context.sampleRate * 0.02);
  const buffer = new Float32Array(step);
  const levels: number[] = [];
  const until = performance.now() + Math.max(0, ms);
  while (performance.now() < until) {
    spectrumProbe.getFloatTimeDomainData(buffer);
    let sum = 0;
    for (let i = 0; i < buffer.length; i++) sum += buffer[i] * buffer[i];
    levels.push(Math.sqrt(sum / buffer.length));
    // Yield so the page (and the audio callbacks driving the analyser) keep running.
    await new Promise<void>((r) => requestAnimationFrame(() => r()));
  }
  if (levels.length === 0) return empty;
  const min = Math.min(...levels);
  const max = Math.max(...levels);
  const mean = levels.reduce((a, b) => a + b, 0) / levels.length;
  return { stepMs: 20, count: levels.length, min, mean, max, ratio: max / Math.max(min, 1e-9), levels };
}

export function audioSampleRate(): number {
  try {
    // The output rate is what the CONTEXT will run at: guessing 48k before the context
    // exists used to build a 48k pipeline for a 44.1k device (~8.8% pitch/speed error for
    // the whole first session). Creating the context alone is enough — its sampleRate is
    // fixed at construction and the context stays suspended until audio is enabled; the
    // output chain itself is built by ensureContext on enable.
    if (!ctx) {
      ctx = createContext();
      installResumeListeners();
    }
    return ctx.sampleRate;
  } catch {
    return 48000;
  }
}

export function routeWorkletPortTo(target: Worker): boolean {
  if (!workletNode || workletPortHandedOff) return false;
  worker?.postMessage({ type: 'detach' });
  const moved = transferOnce(workletNode.port, (port) =>
    target.postMessage({ type: 'worklet-port', port }, [port]));
  if (moved) workletPortHandedOff = true;
  return moved;
}

export function setSdrAudioEnabled(on: boolean): void {
  clearTransitionWatchdog();
  audioTransitionMuted = false;
  enabled = on;
  publishAudioDebug();
  if (on) {
    // Start from a clean ring/resampler so a hand-off cannot replay stale tail audio.
    resetPlayback();
    try {
      ensureContext();
      worker?.postMessage({ type: 'enabled', value: true });
      worker?.postMessage({ type: 'mute', value: false });
      void ctx?.resume();
    } catch {
      // Headless browsers and hosts without an audio device keep the data path harmless.
    }
  } else {
    resetPlayback();
    worker?.postMessage({ type: 'enabled', value: false });
  }
}
