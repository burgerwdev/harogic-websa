// SDR audio playback.
//
// The AudioContext and the AudioWorklet must live on the main thread (Web Audio is not
// available in workers), but the *ingress* does not: a dedicated worker owns an audio-only
// WebSocket, resamples, and drives the worklet's MessagePort directly (the port is
// transferred to it). Neither reception nor delivery therefore depends on the main thread,
// which is what used to make the ring underrun whenever rendering stalled.
//
// Browsers without AudioWorklet keep the ScriptProcessor output, which is main-thread only;
// there the worker posts the resampled buffers back and this module writes the legacy ring.
let ctx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let worker: Worker | null = null;
//: True once the worklet's port has been handed to the WASM DSP worker. Its port can only be
//: transferred once (a second transfer throws "Port at index 0 is already neutered"), so both the
//: handoff and the Python audio worker's own transfer have to respect this.
let workletPortHandedOff = false;
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
function startWorker(port: MessagePort | null): void {
  if (worker) return;
  worker = new Worker(new URL('./sdrAudioWorker.ts', import.meta.url), { type: 'module' });
  worker.onmessage = (event: MessageEvent) => {
    const d = (event.data || {}) as Record<string, any>;
    if (d.type === 'samples' && d.samples) {
      writeLegacy(d.samples as Float32Array);
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
  if (port && !workletPortHandedOff) {
    init.port = port;
    transferOnce(port, (moved) => worker!.postMessage(init, [moved]));
  } else {
    // Without a port (legacy output) or after the DSP took it over: the worker must not be handed a
    // neutered port, and the Python path keeps its socket either way.
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
        workletPortHandedOff = false;
        setupLegacyNode(context);
        worker?.postMessage({ type: 'detach' });
      };
      // Hand the worklet port to the worker: from here on it owns delivery.
      startWorker(candidate.port);
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
  startWorker(null);
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
