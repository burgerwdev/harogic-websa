// SDR baseband ingress worker — the DSP host.
//
// Owns a dedicated `?iq=1` WebSocket, decodes the IQBF frames the backend publishes (the channelized
// baseband its DDC produced, at the DDC's output rate) and runs the Rust/WASM demodulator over them:
// analog modes produce PCM, digital protocols produce text. The PCM goes to the AudioWorklet's port,
// which the main thread transfers here. Neither reception, DSP nor playback delivery touches the main
// thread, so a busy render loop can no longer starve the audio.
//
// The channelization (coarse decimation on the analyzer, fine tuning on the backend) stays on the
// backend, which is what keeps this worker's cost independent of the analyzer's raw IQ rate: the
// device used to hand the browser megabytes per second and the browser had to filter them, which
// could not keep up at the SDR settings the UI offers.
//
// If the pipeline cannot be created (an unknown mode, a module that failed to load) the worker says
// so in its stats and the main thread keeps the Python audio path: the DSP moving to the browser
// must never be the reason there is no audio.
import { decodeFrame, type BasebandFrame } from '../core/frames';
import { dsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';
import { audioCompanionFor, loadPluginManifest } from './registry';

let ws: WebSocket | null = null;
let wsUrl = '';
let wasmUrl = '';
let module: DspModule | null = null;
/// The demodulator: one handle for both paths (analog PCM, digital text).
let pipeline: WasmPipeline | null = null;
/// The analog companion a digital protocol plays its audio through (USB for FT8), or null.
let companion: WasmPipeline | null = null;
/// The shape the companion was last built or reconfigured for (see `sameShape`).
let companionShape: PipelineParams | null = null;
/// The FT8 decoder's own worker: a slot decode blocks for seconds, and doing it here would starve the
/// audio (measured: one underrun and a ~2.5 s gap per slot when both ran in this thread).
let decoder: Worker | null = null;
let decoderReady = false;
let params: PipelineParams | null = null;
let ft8Messages = 0;
/// What the worklet reports about its own ring (relayed to the page's diagnostics).
let workletAvailable = 0;
let workletUnderruns = 0;
let workletReceived = 0;
/// The last failure seen while handing PCM to the worklet ('' when it is fine).
let workletError = '';
let workletRingResets = 0;
/// Samples the worklet had to drop because the producer outran its clock (latency stays bounded).
let workletSlipped = 0;
/// The playback fill's range over the last reporting window: a periodic dip (an audible hitch) shows
/// up here as a min far below the target, which a single sampled value cannot tell you.
let fillMin = Number.POSITIVE_INFINITY;
let fillMax = 0;
/// The worklet's drift-correcting resampling ratio (1 = the producer matches the sound card).
let workletRatio = 1;
/// PCM waiting for a full delivery block, and the block size (20 ms at the output rate).
let pendingPcm: Float32Array[] = [];
let pendingSamples = 0;
let pcmBlockSamples = 960;
let deliveredSamples = 0;
/// Diagnostics: how full a digital decoder's buffer is (a buffer that keeps restarting looks exactly
/// like a quiet band from the outside).
let digitalPushes = 0;
let digitalBuffered = 0;
let digitalResets = 0;
/// Frames handed to the decoder worker (its own counters come back with `ft8-stats`).
let decoderPushes = 0;
/// PCM-level artifact counters: the listener's report was "a periodic puff, obvious on the noise
/// floor" while every *delivery* counter was healthy (0 underruns, 0 slips, the fill holding), so the
/// artifact must be in the samples themselves - either the DSP's chain modulating them or the
/// baseband arriving with holes. These count what the ear notices: a block that is silent when its
/// neighbours were not, and a step between consecutive blocks (a click/discontinuity).
let silentBlocks = 0;
let discontinuities = 0;
/// The PCM level's own range over the window: a noise floor that "breathes" (the AGC pumping, a slow
/// amplitude modulation of the baseband) shows up here as a wide min..max, while a steady stream is a
/// couple of percent.
let pcmRmsMin = Number.POSITIVE_INFINITY;
let pcmRmsMax = 0;
let lastPcmTail = 0;
let pcmBlocks = 0;
/// How often the pipeline was (re)built: a rebuild resets the filters and the resampler, so a counter
/// that climbs while nothing changes is a periodic transient (this is how the once-a-second rebuild -
/// caused by comparing the *measured* baseband rate exactly - was found).
let pipelineBuilds = 0;
/// True while the current mode is a protocol decoder (their audio comes from the companion).
let digital = false;
/// Ids the module declares as digital protocols; the registry is the single source of truth, so a
/// new protocol becomes reachable here without a second list in the worker.
let digitalIds = new Set<string>();
let audioEnabled = true;
/// The listener's audio switch (the worklet's playback gate), separate from the chain switch above.
let audioOn = true;
let volume = 1;
/// Noise reduction (the panel's NR control) and the squelch threshold.
let nr = false;
let nrStrength = 0.6;
let squelchDbfs = -110;
let workletPort: MessagePort | null = null;
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
let dropped = 0;
let flushes = 0;
let pcmFrames = 0;
let pcmSamples = 0;
let rms = 0;
let lastSeq = -1;
let reconnectTimer: number | null = null;
let reconnectDelay = 500;

/** Start the decoder worker (once) and relay its messages to the page. */
function startDecoder(): void {
  if (decoder || typeof Worker === 'undefined') return;
  try {
    decoder = new Worker(new URL('./ft8Worker.ts', import.meta.url), { type: 'module' });
  } catch {
    decoder = null;
    return;
  }
  // A decoder worker that dies takes the decode list with it, silently: surface it like any other
  // DSP failure (the page shows it and can still fall back).
  decoder.onerror = (event: ErrorEvent) => {
    post({ type: 'error', message: `FT8 decoder failed: ${event.message || 'error'}` });
  };
  decoder.onmessage = (event: MessageEvent) => {
    const d = (event.data || {}) as Record<string, any>;
    if (d.type === 'ready') {
      decoderReady = true;
      pushDecoderParams();
      return;
    }
    if (d.type === 'ft8-stats') {
      // The decoder's own counters stand in for the ones this worker used to keep.
      if (typeof d.pushes === 'number') digitalPushes = d.pushes;
      if (typeof d.buffered === 'number') digitalBuffered = d.buffered;
      if (typeof d.decodes === 'number') ft8Messages = d.decodes;
      if (typeof d.skipped === 'number') digitalResets = d.skipped;   // reported as dsp_resets
      postStats();
      return;
    }
    // `ft8` (a decode) and `error` are the shapes the page already reads.
    post(d);
  };
  // Its own socket, so a decode attempt cannot delay the audio (see `ft8Worker`).
  decoder.postMessage({ type: 'init', wasmUrl, url: wsUrl });
}

/** Tell the decoder worker what to decode (and whether to bother). */
function pushDecoderParams(): void {
  if (!decoder || !decoderReady) return;
  const wanted = params && digitalIds.has(params.mode);
  decoder.postMessage({
    type: 'configure',
    enabled: Boolean(wanted),
    params: wanted ? params : null,
  });
}

function post(msg: Record<string, unknown>, transfer?: Transferable[]): void {
  // WorkerGlobalScope.postMessage(message, transferList)
  (self as unknown as { postMessage: (m: unknown, t?: Transferable[]) => void })
    .postMessage(msg, transfer || []);
}

function postStats(): void {
  post({
    type: 'stats', enabled, blocks, samples, rate, centerHz, dropped, flushes,
    pcmFrames, pcmSamples, rms, mode: params?.mode ?? '',
    // "The browser DSP is the audio source": for an analog mode that is the demodulator itself, and
    // for a digital one it is the companion demodulator that plays the channel. The page hands the
    // AudioWorklet port over on this flag, so a digital mode without it never gets heard.
    pipeline: Boolean(pipeline?.ok || companion?.ok),
    ft8Messages, digitalPushes, digitalBuffered, digitalResets, pipelineBuilds,
    worklet: workletPort ? 1 : 0, workletAvailable, workletUnderruns, workletReceived,
    workletError, workletRingResets, workletSlipped, workletRatio,
    fillMin: Number.isFinite(fillMin) ? fillMin : 0, fillMax,
    silentBlocks, discontinuities, pcmBlocks,
    pcmRmsMin: Number.isFinite(pcmRmsMin) ? pcmRmsMin : 0, pcmRmsMax,
    pcmPending: pendingSamples, deliveredSamples, nr, nrStrength, squelchDbfs,
    deemphUs: params?.deemphUs ?? -1, audioOn,
  });
}

/** Create or rebuild the pipeline for the current parameters. */
function syncPipeline(): void {
  if (!enabled || !params || !module) return;
  const isDigital = digitalIds.has(params.mode);
  digital = isDigital;
  if (pipeline && pipeline.mode === params.mode && !isDigital) {
    applyListenerControls();
    syncCompanion(isDigital);
    return;
  }
  if (isDigital) {
    // A protocol decoder lives in its own worker (`decoder`): this thread keeps only the companion
    // demodulator, so a multi-second slot decode cannot starve the audio (measured before the split:
    // one underrun and a ~2.5 s gap per slot).
    pipeline?.free();
    pipeline = null;
  } else if (pipeline) {
    pipeline.reconfigure(params, false);
  } else {
    pipeline = new WasmPipeline(module, params, false);
  }
  if (!isDigital && !pipeline?.ok) {
    // Declared but not runnable: report it instead of pretending it works.
    pipeline?.free();
    pipeline = null;
    post({ type: 'error', message: `no DSP pipeline for mode ${params.mode}` });
    postStats();
    return;
  }
  syncCompanion(isDigital);
  pushDecoderParams();
  applyListenerControls();
  post({ type: 'ready', mode: params.mode });
  postStats();
}

/**
 * The analog companion for a digital protocol (USB for FT8).
 *
 * A decoder produces text and no audio; the operator still wants to hear the channel, so the same
 * baseband is fed to a second handle running an SSB demodulator. Two handles rather than one mixed
 * output keeps the separation rule intact: the decoder reads the baseband untouched, and the audio
 * chain only ever sees the companion's PCM. An analog mode has no companion (it *is* the audio).
 */
/// True when a pipeline already describes this exact channel shape.
///
/// The baseband rate is a *measured* value, so it wobbles by a fraction of a percent between
/// windows; anything smaller than that is the same channel. This is the same rule the main
/// pipeline's rebuild check uses.
function sameShape(current: PipelineParams | null, next: PipelineParams): boolean {
  if (!current) return false;
  if (current.mode !== next.mode) return false;
  if (current.outRate !== next.outRate) return false;
  if (current.ifBw !== next.ifBw) return false;
  if (current.pitch !== next.pitch) return false;
  if (current.deemphUs !== next.deemphUs) return false;
  return Math.abs(current.fsIn - next.fsIn) <= Math.max(200, next.fsIn * 0.01);
}

function syncCompanion(isDigital: boolean): void {
  // The decoder runs next door, not here (see `decoder`).
  pushDecoderParams();
  const wanted = isDigital && params ? audioCompanionFor(params.mode) : null;
  const shape = params
    ? { ...params, mode: wanted ?? '', ifBw: Math.max(3000, Number(params.ifBw) || 0) }
    : null;
  if (!module || !wanted || !shape) {
    companion?.free();
    companion = null;
    companionShape = null;
    return;
  }
  // A `configure` arrives on every STATUS, once a second, and re-sending an unchanged shape used to
  // rebuild the companion: `reconfigure` frees the DSP handle and makes a new one, so the chain lost
  // its state and its output was momentarily silent - a 1 Hz dropout, audible only on a quiet band
  // and only in a digital mode, because only then is there a companion at all. Rebuilding is for a
  // real change; the shape is compared first.
  if (companion && companion.mode === wanted && companion.ok && sameShape(companionShape, shape)) {
    return;
  }
  if (companion && companion.mode === wanted && companion.ok) {
    companionShape = shape;
    companion.reconfigure(shape, false);
    return;
  }
  companion?.free();
  companion = new WasmPipeline(module, shape, false);
  companionShape = shape;
  pipelineBuilds += 1;
  if (!companion.ok) {
    companion.free();
    companion = null;
    post({ type: 'error', message: `no audio companion for mode ${params?.mode}` });
  }
}

/** Push the listener's settings into the pipeline (volume, chain, NR, squelch). */
function applyListenerControls(): void {
  for (const target of [pipeline, companion]) {
    if (!target) continue;
    target.setVolume(volume);
    target.setAudioEnabled(audioEnabled);
    target.setNr(nr, nrStrength);
    target.setSquelch(squelchDbfs);
    target.setDeemph(params?.deemphUs ?? -1);
  }
}

/** Drop the worker's queued PCM and tell the worklet to drop the ring it already holds. */
function resetDelivery(): void {
  pendingPcm = [];
  pendingSamples = 0;
  workletPort?.postMessage({ type: 'reset' });
}

/**
 * Hand PCM to the worklet in ~20 ms blocks.
 *
 * The pipeline emits a small block per baseband frame (a few ms), but the worklet only resumes after
 * an underrun once it holds 20 ms — a stream of tiny bursts makes the ring oscillate around zero and
 * every underrun is an audible fade-out/re-prime. `out_rate` is the AudioWorklet's rate, so one block
 * is always 20 ms of output whatever the device runs at.
 */
function deliver(pcm: Float32Array): void {
  if (pcm.length === 0) return;
  // Artifact counters (see the fields above). A block boundary that jumps is a click; a block that is
  // silent while the stream is running is a hole in the audio.
  let blockSum = 0;
  for (let i = 0; i < pcm.length; i++) blockSum += pcm[i] * pcm[i];
  const blockRms = Math.sqrt(blockSum / pcm.length);
  if (pcmBlocks > 0) {
    if (blockRms < 1e-5) silentBlocks += 1;
    if (Math.abs(pcm[0] - lastPcmTail) > 0.25) discontinuities += 1;
  }
  lastPcmTail = pcm[pcm.length - 1];
  pcmBlocks += 1;
  if (blockRms > 0 && blockRms < pcmRmsMin) pcmRmsMin = blockRms;
  if (blockRms > pcmRmsMax) pcmRmsMax = blockRms;
  let sum = 0;
  for (let index = 0; index < pcm.length; index++) sum += pcm[index] * pcm[index];
  rms = Math.sqrt(sum / pcm.length);
  pcmFrames++;
  pcmSamples += pcm.length;
  if (!workletPort) return;                          // produced, but nobody to hand it to
  pendingPcm.push(pcm);
  pendingSamples += pcm.length;
  if (pendingSamples < pcmBlockSamples) return;
  const block = new Float32Array(pendingSamples);
  let offset = 0;
  for (const part of pendingPcm) {
    block.set(part, offset);
    offset += part.length;
  }
  pendingPcm = [];
  pendingSamples = 0;
  deliveredSamples += block.length;
  // Transferred, so the audio thread never waits for a copy. A failure here used to be silent: the
  // counters keep climbing while nothing reaches the ring, which is exactly the "produced but
  // never heard" shape, so it is recorded and published.
  try {
    workletPort.postMessage({ type: 'samples', samples: block }, [block.buffer]);
    workletError = '';
  } catch (error) {
    workletError = String((error as Error)?.message || error);
    post({ type: 'error', message: `audio delivery failed: ${workletError}` });
  }
}

function resetStream(): void {
  lastSeq = -1;
  flushes++;
  pipeline?.reset();
  resetDelivery();
}

function onBaseband(frame: BasebandFrame): void {
  if (frame.seq === 0) {
    // The backend retuned or reconfigured: the queued blocks and the demodulator's history belong to
    // another channel. A retune only clears the channel history (the level control is kept), while a
    // geometry change rebuilds the whole pipeline through `configure`.
    lastSeq = -1;
    flushes++;
    pipeline?.retune();
    resetDelivery();
    if (frame.samples === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;      // a gap the client never saw (overrun on the wire)
    // A gap tears the buffered slot: decoding it can only fail, and a failed decode costs a whole
    // search. Start the next slot clean instead (the counter above is what makes this visible).
    pipeline?.reset();
    digitalResets += 1;
  }
  if (frame.seq !== 0) lastSeq = frame.seq;
  rate = frame.rate;
  centerHz = frame.centerHz;
  blocks++;
  samples += frame.samples;
  if (digital) {
    // Audio first, *then* hand the samples to the decoder: transferring a buffer detaches the view it
    // came from, and reading a detached view yields length 0 (silence) rather than an error.
    if (companion) deliver(companion.process(frame.iq));
    decoderPushes += 1;
    // The decoder gets the baseband in its own thread; the transfer costs nothing (this frame's
    // buffer, and nothing here needs it afterwards). A slow decode therefore cannot starve the audio.
    if (decoder && decoderReady) {
      try {
        // `at` is the frame's arrival: the decoder worker drops frames that queued up while it was
        // busy (see there), because a decode attempt can take seconds and the backlog must not grow.
        decoder.postMessage({ type: 'frame', iq: frame.iq, centerHz, at: Date.now() },
                            [frame.iq.buffer]);
      } catch {
        decoder?.postMessage({ type: 'frame', iq: frame.iq.slice(), centerHz, at: Date.now() });
      }
    }
  } else if (pipeline) {
    deliver(pipeline.process(frame.iq));
  }
  if (blocks % 25 === 0) postStats();
}

function connect(): void {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  try {
    ws = new WebSocket(wsUrl);
  } catch {
    scheduleReconnect();
    return;
  }
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => { reconnectDelay = 500; };
  ws.onmessage = (event: MessageEvent) => {
    const d = event.data;
    if (!(d instanceof ArrayBuffer)) return;
    const frame = decodeFrame(d);
    if (frame === null || frame.kind !== 'baseband') return;
    onBaseband(frame);
  };
  ws.onclose = () => { ws = null; scheduleReconnect(); };
  ws.onerror = () => { ws?.close(); };
}

function scheduleReconnect(): void {
  if (reconnectTimer !== null) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    reconnectDelay = Math.min(8000, reconnectDelay * 2);
    connect();
  }, reconnectDelay) as unknown as number;
}

self.onmessage = (event: MessageEvent) => {
  const msg = (event.data || {}) as Record<string, any>;
  if (msg.type === 'init') {
    wsUrl = String(msg.url || '');
    wasmUrl = String(msg.wasmUrl || '');
    if (msg.port) {
      workletPort = msg.port as MessagePort;
      workletPort.start();
    }
    startDecoder();
    if (wasmUrl) {
      // `loadPluginManifest` is the registry's loader: it caches the manifest, which the registry's
      // helpers (which modes are protocols, which companion a protocol needs) read. Reading the
      // manifest into a local variable instead left those helpers looking at nothing.
      void loadPluginManifest(wasmUrl)
        .then((plugins) => {
          module = dsp();
          // Which ids are protocols (as opposed to audio demodulators) comes from the module's own
          // plugin manifest: the worker does not carry a list of its own.
          digitalIds = new Set(
            plugins.filter((plugin) => plugin.kind === 'digital').map((plugin) => plugin.id),
          );
          syncPipeline();
        })
        .catch(() => {
          // No module: the stats say so and the Python audio path keeps playing.
          post({ type: 'error', message: 'dsp.wasm unavailable' });
          postStats();
        });
    }
    if (wsUrl) connect();
  } else if (msg.type === 'enabled') {
    enabled = Boolean(msg.value);
    if (!enabled) {
      pipeline?.free();
      pipeline = null;
      companion?.free();
      companion = null;
      resetDelivery();
    }
    syncPipeline();
    postStats();
  } else if (msg.type === 'configure') {
    const next = msg.params as PipelineParams;
    if (next && Number(next.fsIn) > 0 && Number(next.outRate) > 0) {
      // 20 ms at the output rate: the worklet's own re-prime threshold.
      pcmBlockSamples = Math.max(128, Math.round(Number(next.outRate) * 0.02));
      const previous = params;
      params = next;
      if (typeof msg.volume === 'number') volume = msg.volume;
      if (typeof msg.audioEnabled === 'boolean') audioEnabled = msg.audioEnabled;
      if (typeof msg.nr === 'boolean') nr = msg.nr;
      if (typeof msg.nrStrength === 'number') nrStrength = msg.nrStrength;
      if (typeof msg.squelch === 'number') squelchDbfs = msg.squelch;
      // A rate or bandwidth change rebuilds the demodulator (its filters describe the rate they were
      // designed for), but only a *real* change: the baseband rate the backend reports is measured, so
      // it wobbles by a fraction of a percent every window - and a strict comparison rebuilt the whole
      // pipeline once a second, which is an audible transient every second (reported as a weak 1 Hz
      // sound in every mode). An IF-bandwidth change moves the rate by far more than this tolerance.
      const rateChanged =
        !!previous && Math.abs(previous.fsIn - next.fsIn) > Math.max(200, next.fsIn * 0.01);
      const geometryChanged =
        !previous ||
        previous.mode !== next.mode ||
        previous.ifBw !== next.ifBw ||
        rateChanged ||
        previous.outRate !== next.outRate ||
        previous.pitch !== next.pitch;
      if (pipeline && geometryChanged && !digitalIds.has(next.mode)) {
        // The baseband geometry changed: the demodulator's filters describe another channel.
        pipeline.reconfigure(next, false);
        if (!pipeline.ok) {
          pipeline.free();
          pipeline = null;
          post({ type: 'error', message: `no DSP pipeline for mode ${next.mode}` });
        }
      }
      // A digital mode's decoder is rebuilt by its own worker; here only the companion changes.
      syncCompanion(digitalIds.has(next.mode));
      applyListenerControls();
      syncPipeline();
    }
  } else if (msg.type === 'volume') {
    volume = Number(msg.value) || 0;
    pipeline?.setVolume(volume);
  } else if (msg.type === 'audio') {
    audioEnabled = Boolean(msg.value);
    pipeline?.setAudioEnabled(audioEnabled);
  } else if (msg.type === 'audio-enabled') {
    // The listener's audio switch. The worklet owns playback (the browser DSP handed it the port),
    // so the switch has to reach *it*: an empty ring is silence, and a silent worklet is what "Off"
    // means. The chain setting above is separate (it is the RAW/enhancement switch).
    audioOn = Boolean(msg.value);
    workletPort?.postMessage({ type: 'enabled', value: audioOn });
    // While the switch was off the producer kept running and the ring filled to its ceiling: drop
    // that window so switching back on plays the live stream instead of the recent past.
    if (audioOn) workletPort?.postMessage({ type: 'reset' });
    postStats();
  } else if (msg.type === 'nr') {
    nr = Boolean(msg.enabled);
    if (typeof msg.strength === 'number') nrStrength = msg.strength;
    pipeline?.setNr(nr, nrStrength);
    postStats();
  } else if (msg.type === 'squelch') {
    squelchDbfs = Number(msg.value) || -110;
    pipeline?.setSquelch(squelchDbfs);
    postStats();
  } else if (msg.type === 'deemph') {
    params = params ? { ...params, deemphUs: Number(msg.value) } : params;
    pipeline?.setDeemph(Number(msg.value));
    postStats();
  } else if (msg.type === 'worklet-port') {
    // The main thread handed the worklet over once this worker reported a running pipeline.
    const port = msg.port as MessagePort | undefined;
    if (port) {
      workletPort = port;
      workletPort.onmessage = (event: MessageEvent) => {
        const status = (event.data || {}) as Record<string, any>;
        if (status.type !== 'status') return;
        workletAvailable = Number(status.available) || 0;
        workletUnderruns = Number(status.underruns) || 0;
        workletReceived = Number(status.received) || 0;
        workletRingResets = Number(status.resets) || 0;
        workletSlipped = Number(status.slipped) || 0;
        workletRatio = Number(status.ratio) || 1;
        const fill = Number(status.available) || 0;
        fillMin = Math.min(fillMin, fill);
        fillMax = Math.max(fillMax, fill);
      };
      workletPort.start();
      workletPort.postMessage({ type: 'enabled', value: enabled });
    }
  } else if (msg.type === 'reset') {
    resetStream();
  } else if (msg.type === 'detach') {
    workletPort = null;
    pendingPcm = [];
    pendingSamples = 0;
  }
};

post({ type: 'ready' });
