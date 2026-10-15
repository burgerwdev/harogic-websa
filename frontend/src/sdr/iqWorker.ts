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
import { audioCompanionFor, loadPluginManifest, sstvModeTable } from './registry';
import { GgMorseEngine } from '../dsp/ggmorseEngine';

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
/// The FT8 decoder's workers: a slot decode blocks for seconds, and doing it here would starve the
/// audio (measured: one underrun and a ~2.5 s gap per slot when both ran in this thread). The
/// decoder is two workers (sdr/ft8Worker.ts): `decoderStream` owns the socket so a decode can
/// never stall it, `decoder` owns the WASM pipeline and receives the stream over a MessagePort.
let decoderStream: Worker | null = null;
let decoder: Worker | null = null;
let decoderReady = false;
let params: PipelineParams | null = null;
/// The CW (Morse) decoder, alive only while CW is the demodulator: ggmorse in wasm, fed the same
/// PCM the listener hears. Measured at 2 ms per 100 ms of audio (worst 13 ms), so it runs inline
/// here - unlike the FT8 slot decode, which takes seconds and needed a worker of its own.
let cwDecoder: GgMorseEngine | null = null;
/// The CW decoder's state, the way the DFN stage reports its own: 'off' (not this mode), 'loading'
/// (the wasm module is on its way) or 'ready'. Published always, because the interesting case is a
/// decoder that produces *nothing* - a counter that only appears once characters arrive cannot show
/// that (which is exactly how a first mode switch that never built the decoder stayed invisible).
let cwState: 'off' | 'loading' | 'ready' = 'off';
/// Bumped on every (re)build of the CW decoder: an async load installs its engine only while it is
/// still the latest request.
///
/// Comparing the params *object* instead did not work, and the symptom was exactly "CW never comes
/// back": a STATUS arrives every second with a fresh params object, so the first load's engine was
/// always judged stale, freed, and the decoder stayed null after the operator switched away from CW
/// and back (reported).
let cwGeneration = 0;
/// Characters the CW decoder has produced (diagnostics: a CW session has no other evidence).
let cwChars = 0;
let ft8Messages = 0;
/// What the worklet reports about its own ring (relayed to the page's diagnostics).
let workletAvailable = 0;
let workletUnderruns = 0;
/// DIAG: the worklet's playback gate, so a retune's audio gap is measurable from the page.
let workletPlaying = 0;
let workletPrimed = 0;
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
/// DRM's own path: how often the worker fed the DRM decoder, and how often it answered.
/// Without these two the diagnostics cannot tell "no decoder" from "no lock".
let drmPushes = 0;
let drmReports = 0;
/// SSTV's own path: blocks pushed into the decoder and pictures posted to the page. The two
/// remembered status fields are what keep a block that decoded nothing new from re-sending a
/// quarter-megabyte picture.
let sstvPushes = 0;
let sstvImages = 0;
let sstvSerial = -1;
let sstvLines = -1;
/// The decoder state last posted, so the page is told only when it changes (an attempt is the event
/// worth reporting; the buffered seconds ride along with it).
let sstvAttempts = -1;
let sstvHasPicture = false;
/// The frequency the current SSTV picture belongs to (a flush on the same one keeps it).
let sstvHz = 0;
/// Raw-baseband RMS, EMA over blocks (-1 = nothing measured yet).
let sstvSrcRms = -1;
let digitalBuffered = 0;
let digitalResets = 0;
/// Resets reported by the decoder worker itself (its own window thrown away). Kept apart from
/// `digitalResets`, which is this worker's companion pipeline: they were the same variable, so the
/// decoder's resets overwrote the audio path's and neither was readable.
let decoderResets = 0;
//: Kept so "no decode" can be told apart from "no attempt". The decoder computes these and they
//: were being discarded here, which left the decode table as the only evidence -- and it cannot
//: distinguish a slot that was skipped from one that was attempted and failed.
let digitalAttempts = 0;
let digitalDropped = 0;
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
/// Samples of the DRM audio stream already delivered (the WASM receiver accumulates PCM across
/// blocks, so the worker only forwards the suffix it has not sent yet). Reset on retune.
let audioEnabled = true;
/// The listener's audio switch (the worklet's playback gate), separate from the chain switch above.
let audioOn = true;
let volume = 1;
/// Noise reduction (the panel's NR control) and the squelch threshold.
let nr = false;
let nrStrength = 0.6;
/// DeepFilterNet3's attenuation limit in dB (0 = passthrough, higher = stronger denoising).
let nrAtten = 6;
/// The noise-reduction algorithm: 'wiener' (WASM) or 'dfn' (browser DeepFilterNet3 stage).
let nrAlgo: 'wiener' | 'dfn' = 'wiener';
/// The DeepFilterNet3 worker (own thread: the tract WASM blocks ~5.6 ms/hop and must not starve
/// this worker's WebSocket receive on wide modes like WFM 180 kHz).
let dfnWorker: Worker | null = null;
let dfnReady = false;
let dfnLoading = false;
let dfnError = '';
/// True while the listener asked for dfn but the WASM Wiener is standing in for it.
let dfnFellBack = false;
/// Monotonic sequence for ordering the async dfn worker responses.
let dfnSeq = 0;
let squelchDbfs = -110;
let workletPort: MessagePort | null = null;
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
/// DIAG: the last `configure` the worker applied, and the rate the frames arrive at, so a repro
/// that loses the side chain can be read back even after `wfm-info` stops.
let lastConfig = { fsIn: 0, mode: '', ifBw: 0, at: 0 };
/// DIAG/guard: the frame rate already reported to the main thread, so a persistent mismatch sends
/// one message, not one per frame.
let reportedRate = 0;
/// Samples in the WFM scope snapshot the decode pane draws (about 10 ms at 48 kHz).
const SCOPE_SAMPLES = 512;
/// The scope snapshot buffer: filled per block and copied into the message, so a posting that
/// the main thread reads late cannot see a half-written block.
const scopeL = new Float32Array(SCOPE_SAMPLES);
const scopeR = new Float32Array(SCOPE_SAMPLES);
/// Audio level readout: a peak-hold that decays like a broadcast peak meter (about 20 dB/s), so
/// the number holds the peak the listener hears instead of following every block. A bare
/// per-block peak jumps by many dB between blocks and makes the readout unreadable.
let audioPeakHoldDb = -200;
const AUDIO_HOLD_DECAY_DB = 0.4;
/// Audio level readout, VU style: a 300 ms mean-square average (the integration a VU meter uses),
/// smoothed per block. A per-block peak follows every syllable and is unreadable; the peak hold
/// above is kept for the scope's vertical scale, which wants the loudest recent excursion.
let audioVuMs = 0;
const VU_TAU_S = 0.3;
const BLOCK_S = 0.02;
let dropped = 0;
let flushes = 0;
let pcmFrames = 0;
let pcmSamples = 0;
let rms = 0;
let lastSeq = -1;
let reconnectTimer: number | null = null;
let reconnectDelay = 500;

/** Start the decoder workers (once) and relay their messages to the page. */
function startDecoder(): void {
  if (decoder || typeof Worker === 'undefined') return;
  try {
    decoderStream = new Worker(new URL('./ft8Worker.ts', import.meta.url), { type: 'module' });
    decoder = new Worker(new URL('./ft8DecodeWorker.ts', import.meta.url), { type: 'module' });
  } catch {
    decoder = null;
    decoderStream = null;
    return;
  }
  // A decoder worker that dies takes the decode list with it, silently: surface it like any other
  // DSP failure (the page shows it and can still fall back).
  const fail = (event: ErrorEvent) => {
    post({ type: 'error', message: `FT8 decoder failed: ${event.message || 'error'}` });
  };
  decoderStream.onerror = fail;
  decoder.onerror = fail;
  // The stream worker relays the decode worker's messages (ready/ft8/ft8-stats/error), so the
  // message handling below is the same as when the halves were one worker.
  decoderStream.onmessage = (event: MessageEvent) => {
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
      if (typeof d.resets === 'number') decoderResets = d.resets;
      if (typeof d.attempts === 'number') digitalAttempts = d.attempts;
      if (typeof d.dropped === 'number') digitalDropped = d.dropped;
      postStats();
      return;
    }
    // `ft8` (a decode) and `error` are the shapes the page already reads.
    if (d.type === 'ft8' && params?.mode !== 'ft8') return;
    post(d);
  };
  // The two halves, wired with a MessageChannel: the stream worker reads the socket forever, the
  // decode worker runs the WASM at its own pace, and neither can stall the other. Its own socket,
  // so a decode attempt cannot delay the audio either (see `ft8Worker`).
  const channel = new MessageChannel();
  decoderStream?.postMessage({ type: 'init', url: wsUrl, port: channel.port1 }, [channel.port1]);
  decoder.postMessage({ type: 'init', wasmUrl, port: channel.port2 }, [channel.port2]);
}

/** Tell the decoder worker what to decode (and whether to bother). */
function pushDecoderParams(): void {
  if (!decoder || !decoderReady) return;
  const wanted = enabled && params?.mode === 'ft8';
  decoderStream?.postMessage({ type: 'active', enabled: Boolean(wanted) });
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

function postStats(): void {  post({
    type: 'stats', enabled, blocks, samples, rate, centerHz, dropped, flushes,
    cfgFsIn: Math.round(lastConfig.fsIn), cfgMode: lastConfig.mode, cfgIfBw: lastConfig.ifBw,
    cfgAt: lastConfig.at, frameRate: Math.round(rate),
    pcmFrames, pcmSamples, rms, mode: params?.mode ?? '',
    digital: digital ? 1 : 0, fsIn: Number(params?.fsIn) || 0, outRate: Number(params?.outRate) || 0,
    drmPushes, drmReports, digitalIdCount: digitalIds.size,
    // "The browser DSP is the audio source": for an analog mode that is the demodulator itself, and
    // for a digital one it is the companion demodulator that plays the channel. The page hands the
    // AudioWorklet port over on this flag, so a digital mode without it never gets heard.
    pipeline: Boolean(pipeline?.ok || companion?.ok),
    ft8Messages, digitalPushes, digitalBuffered, digitalResets, decoderResets, pipelineBuilds,
    cwChars, cw: cwState,
    sstvPushes, sstvImages, sstvAttempts, sstvHasPicture,
    digitalAttempts, digitalDropped,
    worklet: workletPort ? 1 : 0, workletAvailable, workletUnderruns, workletReceived,
    workletPlaying, workletPrimed,
    workletError, workletRingResets, workletSlipped, workletRatio,
    fillMin: Number.isFinite(fillMin) ? fillMin : 0, fillMax,
    silentBlocks, discontinuities, pcmBlocks,
    pcmRmsMin: Number.isFinite(pcmRmsMin) ? pcmRmsMin : 0, pcmRmsMax,
    pcmPending: pendingSamples, deliveredSamples, nr, nrStrength, nrAlgo, squelchDbfs,
    dfn: dfnReady ? 'ready' : dfnLoading ? 'loading' : dfnError ? 'fallback' : 'off',
    dfnError,
    deemphUs: params?.deemphUs ?? -1, audioOn,
    stereo: workletStereo ? 1 : 0,
    procMs: Math.round(procMsAccum * 10) / 10,
  });
  procMsAccum = 0;
  if (params?.mode === 'sstv') postSstvState();
}

/**
 * The SSTV decoder's state, posted with the stats.
 *
 * The pane's empty state reads this, and the two numbers that matter are `running` and `pushes`: a
 * decoder whose pipeline was rejected (a stale artifact, an unknown mode) never pushes at all, which
 * is a different fault from a decoder that pushes and finds no signal — and to the operator the two
 * looked identical before this line existed.
 */
function postSstvState(): void {
  const decoder = pipeline?.decodeState() ?? null;
  post({
    type: 'sstv-state',
    running: Boolean(pipeline?.ok),
    pushes: sstvPushes,
    attempts: decoder?.attempts ?? 0,
    bufferedSamples: decoder?.bufferedSamples ?? 0,
    bufferedSeconds: decoder?.bufferedSeconds ?? 0,
    hasPicture: decoder?.hasPicture ?? false,
    level: decoder?.level ?? 0,
    srcLevel: sstvSrcRms < 0 ? 0 : sstvSrcRms,
  });
}

/// The rate and pitch the CW decoder was built for (see `syncCwDecoder`).
let cwShape: { rate: number; pitch: number } | null = null;
/// How far the Pitch may move before the decoder is rebuilt: ggmorse takes the search range in its
/// constructor, so a new Pitch means a new decoder - but the operator drags that control, and a
/// rebuild per step of the drag would restart the decoder under the hand. Twenty Hz is well inside
/// the tolerance the decoder searches anyway.
const CW_PITCH_STEP_HZ = 20;

/**
 * Keep the CW decoder in step with the audio chain's rate and the operator's Pitch.
 *
 * Deliberately *not* part of `syncPipeline`: that function returns early when the pipeline already
 * describes the requested mode, and the `configure` handler reconfigures the pipeline *before*
 * calling it - so on the first switch to CW the pipeline was already CW, `syncPipeline` returned
 * early, and the decoder was never built. The mode looked right everywhere (the panel, the
 * backend, `dsp_mode=cw`) while nothing was ever decoded, and only a visit to another mode and back
 * fixed it (reported, then measured: every later switch worked, the first one never did).
 *
 * Idempotent, like `syncDfn` and `syncCompanion`: it is called from every configuration path, and it
 * only rebuilds when the shape it was built for actually changed.
 */
function syncCwDecoder(): void {
  const wanted = !!params && !digitalIds.has(params.mode) && params.mode === 'cw';
  if (!wanted) {
    if (cwDecoder || cwState !== 'off') {
      cwDecoder?.free();
      cwDecoder = null;
      cwShape = null;
      cwState = 'off';
      cwGeneration++;
      postStats();
    }
    return;
  }
  const shape = { rate: params!.outRate, pitch: params!.pitch };
  if (cwDecoder && cwShape
      && cwShape.rate === shape.rate
      && Math.abs(cwShape.pitch - shape.pitch) < CW_PITCH_STEP_HZ) {
    return;
  }
  cwDecoder?.free();
  cwDecoder = null;
  cwShape = shape;
  cwState = 'loading';
  const request = ++cwGeneration;
  void GgMorseEngine.load(shape.rate, shape.pitch)
    .then((engine) => {
      // A reconfigure happened while the wasm loaded: this request is stale, so drop it rather
      // than feed it with the wrong geometry (see `cwGeneration`).
      if (request !== cwGeneration) { engine.free(); return; }
      cwDecoder = engine;
      cwState = 'ready';
      postStats();
    })
    .catch((error: unknown) => {
      if (request !== cwGeneration) return;
      cwState = 'off';
      post({ type: 'error', message: `CW decoder failed: ${String((error as Error)?.message || error)}` });
    });
  postStats();
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
    if (params.mode === 'drm' || params.mode === 'drmplus' || params.mode === 'sstv') {
      // These decoders live in the wasm digital path itself (there is no separate worker), so this
      // thread keeps a *digital* pipeline and feeds it baseband via `push`. A rebuild is required
      // whenever any constructor parameter moves: an IQ-bandwidth change alters the DDC rate, and
      // reusing a plugin built for the old rate feeds it samples at a rate it never sees (the bench
      // failure: after an IQ BW change the SSTV decode stayed dead until a page reload).
      if (pipeline && pipeline.isDigital && sameShape(pipeline.params, params)) {
        applyListenerControls();
        syncCompanion(isDigital);
        return;
      }
      pipeline?.free();
      pipeline = new WasmPipeline(module, params, true);
    } else {
      // A protocol decoder lives in its own worker (`decoder`): this thread keeps only the companion
      // demodulator, so a multi-second slot decode cannot starve the audio (measured before the
      // split: one underrun and a ~2.5 s gap per slot).
      pipeline?.free();
      pipeline = null;
    }
  } else if (pipeline) {
    pipeline.reconfigure(params, false);
  } else {
    pipeline = new WasmPipeline(module, params, false);
  }
  if (!pipeline?.ok) {
    // Declared but not runnable: report it instead of pretending it works.
    pipeline?.free();
    pipeline = null;
    post({ type: 'error', message: `no DSP pipeline for mode ${params.mode}` });
    postStats();
    return;
  }
  syncCwDecoder();
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
  // The WASM Wiener is the NR only when the algorithm is Wiener, or when dfn was requested but
  // has fallen back (load failure, wrong rate, or too slow) — the two never stack.
  const wasmNr = nr && (nrAlgo === 'wiener' || dfnFellBack);
  for (const target of [pipeline, companion]) {
    if (!target) continue;
    target.setVolume(volume);
    target.setAudioEnabled(audioEnabled);
    target.setNr(wasmNr, nrStrength);
    target.setSquelch(squelchDbfs);
    target.setDeemph(params?.deemphUs ?? -1);
  }
}

/** Bring the DeepFilterNet3 worker into line with the listener's NR selection. */
function syncDfn(): void {
  const wanted = nr && nrAlgo === 'dfn';
  if (!wanted) {
    // Stop forwarding; keep the worker for reuse. A later on→cycle re-inits it (fresh stream).
    dfnReady = false;
    dfnLoading = false;
    dfnFellBack = false;
    dfnError = '';
    return;
  }
  if ((params?.outRate ?? 0) !== 48000) {
    dfnFellBack = true;
    dfnError = `rate ${params?.outRate}`;
    applyListenerControls();
    post({ type: 'dfn-status', state: 'fallback', reason: dfnError });
    return;
  }
  // Spawn the dedicated worker once; it caches the model and reports 'ready' when it can process.
  if (!dfnWorker) {
    try {
      dfnWorker = new Worker(new URL('./dfnWorker.ts', import.meta.url), { type: 'module' });
    } catch {
      dfnWorker = null;
      dfnFellBack = true;
      dfnError = 'no Worker support';
      applyListenerControls();
      post({ type: 'dfn-status', state: 'fallback', reason: dfnError });
      return;
    }
    dfnWorker.onerror = (event: ErrorEvent) => {
      dfnFellBack = true;
      dfnReady = false;
      dfnLoading = false;
      dfnError = String(event.message || 'dfn worker failed');
      applyListenerControls();
      post({ type: 'dfn-status', state: 'fallback', reason: dfnError });
    };
    dfnWorker.onmessage = (event: MessageEvent) => {
      const d = (event.data || {}) as Record<string, any>;
      if (d.type === 'ready') {
        dfnLoading = false;
        // The listener may have switched NR off (or to Wiener) while the model was loading.
        if (!(nr && nrAlgo === 'dfn')) return;
        dfnReady = true;
        dfnFellBack = false;
        post({ type: 'dfn-status', state: 'ready' });
      } else if (d.type === 'error') {
        dfnLoading = false;
        if (!(nr && nrAlgo === 'dfn')) return;
        dfnFellBack = true;
        dfnReady = false;
        dfnError = String(d.message || 'dfn error');
        applyListenerControls();
        post({ type: 'dfn-status', state: 'fallback', reason: dfnError });
      } else if (d.type === 'pcm') {
        const block = d.out as Float32Array;
        deliver(workletStereo ? duplicateStereo(block) : block);
      }
    };
  }
  // (Re-)initialize whenever the worker exists but is not ready — freshly spawned, or after an NR
  // off→on cycle (the off-branch cleared `dfnReady`). Guarded by `dfnLoading` so a configure and an
  // nr message in quick succession don't double-init.
  if (!dfnReady && !dfnLoading) {
    dfnLoading = true;
    dfnFellBack = false;
    post({ type: 'dfn-status', state: 'loading' });
    dfnWorker.postMessage({ type: 'init', atten: nrAtten });
  }
}

/** Forward one PCM block to the dfn worker (async: the response arrives on its 'pcm' message). */
function deliverViaDfn(pcm: Float32Array): void {
  if (!dfnWorker || !dfnReady) return;
  dfnWorker.postMessage({ type: 'process', seq: dfnSeq++, pcm }, [pcm.buffer]);
}

/** Drop the worker's queued PCM and tell the worklet to drop the ring it already holds. */
/// DIAG: the last gate state forwarded (transition detection).
let lastGatePlaying = 0;
let lastGateUnderruns = 0;

function resetDelivery(dropRing = true, fast = false): void {
  pendingPcm = [];
  pendingSamples = 0;
  // The layout is per-mode state and survives the flush; the worklet keeps its layout across a
  // ring reset, so the pair stays consistent without another message.
  // The dfn stream's lookahead buffer holds the previous channel: flush it too, so a retune does
  // not play a burst of the old station through the fresh channel.
  dfnWorker?.postMessage({ type: 'reset' });
  if (dropRing) workletPort?.postMessage({ type: 'reset' });
  // A retune re-primes on a short buffer: the double reset plus the cold-start prime held the
  // audio silent for ~1 s (measured; the target is <= 0.5 s).
  if (fast) workletPort?.postMessage({ type: 'fast-prime' });
}

/// Whether the delivered blocks are interleaved L/R pairs (the worklet's ring layout follows).
let workletStereo = false;
/// DSP wall time (ms) accumulated since the last stats window: the live side-chain cost the
/// operator (and the bench) can read next to the 1 Hz budget.
let procMsAccum = 0;

/**
 * Interleave the mono (L+R) and the side (L−R) samples into L/R pairs: L = M + S, R = M − S.
 * The two inputs are aligned one-to-one by the demodulator (same rate, same block cadence); a
 * short side trims the output to what is safely paired.
 */
function interleaveStereo(mono: Float32Array, side: Float32Array): Float32Array<ArrayBuffer> {
  const pairs = Math.min(mono.length, side.length);
  const out = new Float32Array(pairs * 2);
  for (let i = 0; i < pairs; i++) {
    const m = mono[i];
    const s = side[i];
    out[2 * i] = m + s;
    out[2 * i + 1] = m - s;
  }
  return out;
}

/** Duplicate the mono block as both channels (the mono station's stereo layout). */
function duplicateStereo(mono: Float32Array): Float32Array<ArrayBuffer> {
  const out = new Float32Array(mono.length * 2);
  for (let i = 0; i < mono.length; i++) {
    out[2 * i] = mono[i];
    out[2 * i + 1] = mono[i];
  }
  return out;
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
    // The step detector reads consecutive ring values; an interleaved L/R pair alternates by
    // design, so the check only means something in the mono layout.
    if (!workletStereo && Math.abs(pcm[0] - lastPcmTail) > 0.25) discontinuities += 1;
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
  // The 20 ms target counts ring values: a stereo pair is two values per frame.
  const blockTarget = workletStereo ? pcmBlockSamples * 2 : pcmBlockSamples;
  if (pendingSamples < blockTarget) return;
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

/** Linear-interpolation resampler for the DRM audio path (12/24 kHz core → 48 kHz audio). */
function resampleLinear(pcm: Float32Array, srcRate: number, dstRate: number): Float32Array {
  if (srcRate <= 0 || dstRate <= 0 || srcRate === dstRate) return pcm;
  const ratio = srcRate / dstRate;
  const outLen = Math.floor(pcm.length / ratio);
  const out = new Float32Array(outLen);
  for (let i = 0; i < outLen; i++) {
    const pos = i * ratio;
    const i0 = Math.floor(pos);
    const frac = pos - i0;
    const i1 = Math.min(i0 + 1, pcm.length - 1);
    out[i] = pcm[i0] * (1 - frac) + pcm[i1] * frac;
  }
  return out;
}

function resetStream(): void {
  lastSeq = -1;
  flushes++;
  pipeline?.reset();
  if (params?.mode === 'drm' || params?.mode === 'drmplus') post({ type: 'drm-clear' });
  if (params?.mode === 'sstv') {
    sstvSerial = -1;
    sstvLines = -1;
    sstvAttempts = -1;
    sstvHasPicture = false;
    post({ type: 'sstv-clear' });
  }
  resetDelivery();
}

/// DIAG: set window.__wfmDumpIQ=1 from the bench to stash the next baseband frames (the
/// exact samples the decoder consumed) for offline replay; read them back via __wfmIqDump.
let dumpIq: { on: boolean, frames: number[][] } = { on: false, frames: [] };

async function onBaseband(frame: BasebandFrame): Promise<void> {
  // The FRAMES are the authority for the demodulator's geometry. `fsIn` arrives from the backend's
  // STATUS window, and a listen change makes the backend re-plan its channelizer — measured: 300.4
  // to 260.3 kSps — without waiting for that window. A demodulator built for the old rate scales
  // every filter with it, so the pilot's 19 kHz lands 13% low and the decode window reads MONO;
  // only a full re-tune (which restarts the stream) recovered it. Re-running the configure path
  // with the rate the samples actually arrive at keeps that invariant in one place.
  if (params && frame.rate > 0
      && Math.abs(frame.rate - params.fsIn) > Math.max(200, params.fsIn * 0.01)
      && frame.rate !== reportedRate) {
    reportedRate = frame.rate;
    // The rebuild itself belongs to the main thread's configure path: running it here re-entered the
    // pipeline's own lifecycle from inside a frame and trapped the wasm (measured: `unreachable`).
    post({ type: 'geometry', fsIn: frame.rate });
  }
  if (dumpIq.on && dumpIq.frames.length < 160) {
    dumpIq.frames.push(Array.from(frame.iq));
    if (dumpIq.frames.length === 160) {
      dumpIq.on = false;
      post({ type: 'iq-dump', rate: frame.rate, centerHz: frame.centerHz, frames: dumpIq.frames });
    }
  }
  if (frame.seq === 0) {
    // The backend retuned or reconfigured: the queued blocks and the demodulator's history belong to
    // another channel. A retune only clears the channel history (the level control is kept), while a
    // geometry change rebuilds the whole pipeline through `configure`.
    lastSeq = -1;
    flushes++;
    pipeline?.retune();
    if (params?.mode === 'drm' || params?.mode === 'drmplus') post({ type: 'drm-clear' });
      if (params?.mode === 'sstv' && frame.centerHz !== sstvHz) {
      // Only a *moved* channel invalidates the picture. The backend also flushes on a
      // reconfiguration of the same frequency (a filter change, a level step), and clearing the pane
      // on those threw away a picture that was still arriving.
      sstvSerial = -1;
      sstvLines = -1;
      post({ type: 'sstv-clear' });
    }
    resetDelivery(true, true);
    if (frame.samples === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;      // a gap the client never saw (overrun on the wire)
    // A gap tears the buffered slot: decoding it can only fail, and a failed decode costs a whole
    // search. Start the next slot clean instead (the counter above is what makes this visible).
    pipeline?.reset();
    if (params?.mode === 'drm' || params?.mode === 'drmplus') post({ type: 'drm-clear' });
      if (params?.mode === 'sstv') {
      sstvSerial = -1;
      sstvLines = -1;
      post({ type: 'sstv-clear' });
    }
    resetDelivery();
    digitalResets += 1;
  }
  if (frame.seq !== 0) lastSeq = frame.seq;
  rate = frame.rate;
  centerHz = frame.centerHz;
  if (params?.mode === 'sstv') sstvHz = centerHz;
  blocks++;
  samples += frame.samples;
  if (digital) {
    // Audio first (the decoder has its own socket and its own thread - see `ft8Worker`); nothing
    // here touches the samples afterwards, but the ordering keeps the deliver-then-hand-off rule
    // that a transferred buffer demands.
    if (companion) deliver(companion.process(frame.iq));
    if (pipeline && (params?.mode === 'drm' || params?.mode === 'drmplus')) {
      drmPushes += 1;
      const reports = pipeline.push(frame.iq);
      if (reports.length) {
        drmReports += 1;
        post({ type: 'drm', lines: reports.map((report) => report.text), snrDb: reports[0].snrDb });
        const points = pipeline.constellation(1024);
        post({ type: 'drm-constellation', points });
      }
      // DRM audio: the receiver emits PCM in a burst once it has locked and deframed a super
      // frame. audioPcm drains — it returns exactly what accumulated since the previous call
      // (an offset-tracked head-window read stopped delivering once the receiver's buffer
      // outgrew the read cap, which silenced every session after ~1.4 s). Resampled to the
      // worklet's rate (the DRM core runs at 12/24 kHz, the audio chain at 48 kHz).
      const fresh = pipeline.audioPcm(262144);
      if (fresh.length) {
        const srcRate = pipeline.audioRate();
        const dstRate = Number(params?.outRate) || 48000;
        deliver(srcRate === dstRate ? fresh : resampleLinear(fresh, srcRate, dstRate));
      }
    }
    if (pipeline && params?.mode === 'sstv') {
      // SSTV decodes a picture, not audio: `push` drives the decoder and returns its readout lines
      // (a new transmission, and its completion). The picture and its state are read back only when
      // the decoder produced something new — the state is stable between attempts, and the pixels
      // are a quarter of a megabyte.
      sstvPushes += 1;
      // Raw-baseband level of this block (EMA). Posted beside the decoder's own audio level: if the
      // frames carry a signal but the decoder's buffer reads zero, the fault is in the decoder's front
      // end; if the frames are silence, it is upstream.
      {
        let sum = 0;
        for (let i = 0; i < frame.iq.length; i++) sum += frame.iq[i] * frame.iq[i];
        // The backend sends the baseband in the i16 domain (RMS ~45..64), the fake backend in
        // normalized floats: scale the i16 domain down so the readout is a 0..1 level either way.
        const raw = Math.sqrt(sum / frame.iq.length);
        const rms = raw > 1.5 ? raw / 32768 : raw;
        sstvSrcRms = sstvSrcRms < 0 ? rms : sstvSrcRms * 0.9 + rms * 0.1;
      }
      const lines = pipeline.push(frame.iq);
      if (lines.length) post({ type: 'sstv', lines: lines.map((report) => report.text) });
      const status = pipeline.imageStatus();
      if (status && (status.serial !== sstvSerial || status.lines !== sstvLines)) {
        const picture = pipeline.image();
        if (picture) {
          sstvSerial = status.serial;
          sstvLines = status.lines;
          sstvImages += 1;
          post(
            {
              type: 'sstv-image',
              width: picture.width,
              height: picture.height,
              rgb: picture.rgb.buffer,
              meta: {
                name: sstvModeTable()?.[status.modeIndex]?.name ?? '',
                modeIndex: status.modeIndex,
                vis: status.vis,
                lines: status.lines,
                progress: status.progress,
                complete: status.complete,
                serial: status.serial,
                hz: centerHz,
              },
            },
            [picture.rgb.buffer],
          );
        }
      }
      // The decoder's own counters are posted with the stats (see `postSstvState`): a *dead* pipeline
      // has no state to read, and that is exactly the case the pane must be able to report.
    }
    decoderPushes += 1;
  } else if (pipeline) {
    const procStart = performance.now();
    const pcm = pipeline.process(frame.iq);
    // WFM stereo: the side chain's L−R rides with the mono as one interleaved block. The layout
    // is a per-mode constant (never per block — a flip would make the worklet re-read its ring
    // in the wrong layout, which is audible as loud garbage), so a block without side samples
    // duplicates the mono and keeps the layout.
    // One drain per block: side() empties the side buffer, so a second call would return
    // nothing and silently drop the L−R channel.
    const side = pipeline.side();
    let out = pcm;
    if (workletStereo) {
      // The side only reaches the mix while the pilot is detected: the drained samples exist for
      // every WFM block, but without a pilot they are the product detector's noise floor, and a
      // real receiver blends to mono in exactly that state. The LAYOUT stays stereo either way —
      // duplicateStereo keeps the pair, so the worklet never re-reads its ring in the other
      // layout.
      const live = side.length > 0 && pipeline.stereo();
      out = live ? interleaveStereo(pcm, side) : duplicateStereo(pcm);
    }
    // The same PCM the listener gets (the pipeline's own NR included; the DFN stage further down
    // is for the ear). Decoded characters go straight to the page's CW log.
    if (cwDecoder && pcm.length) {
      const chunk = cwDecoder.push(pcm);
      if (chunk) {
        cwChars += chunk.text.length;
        // Always sent: the text may be empty while the meter, the lamp and the per-character
        // confidence (the gate's share) still have something to say about the block.
        post({
          type: 'cw', text: chunk.text, endsLine: chunk.endsLine, share: chunk.share,
          rms: chunk.rms, keyed: chunk.keyed,
        });
      }
    }
    // WFM decode window: post the stereo state and audio level (the pane reads them live).
    if (params?.mode === 'wfm' && pcm.length > 0) {
      // The meter and the scope's vertical scale follow what the LISTENER hears: the interleaved
      // L/R pair when the stereo layout is on, the mono otherwise. Measuring the mono while the
      // worklet gets L = M + S understates the level (a fully separated programme peaks at nearly
      // twice the sum) and the scope would clip its lane on exactly the broadcasts it is there for.
      const metered = workletStereo ? out : pcm;
      let peak = 0.0;
      for (let i = 0; i < metered.length; i++) peak = Math.max(peak, Math.abs(metered[i]));
      const info = pipeline.stereoInfo();
      if (info) {
        // The text rides along only once RDS has locked: before that every call returns the
        // blank state, and the pane shows its placeholder either way.
        const text = info.rdsLocked ? pipeline.rdsText() : null;
        // A short scope snapshot of the audio the listener hears: the pane draws it directly.
        // 512 frames is ~10 ms at 48 kHz, enough to see the waveform rather than a blur. Both
        // channels are snapshotted (they are identical for a mono station), so a stereo
        // broadcast can be shown as two traces. The window starts at a rising zero crossing of
        // the left channel (the scope trigger every instrument uses): without it the trace slides
        // sideways every block and the waveform only blurs.
        const frames = workletStereo ? Math.floor(out.length / 2) : out.length;
        const at = (f: number): number => (workletStereo ? out[2 * f] : out[f]);
        const want = Math.max(0, frames - SCOPE_SAMPLES);
        let start = want;
        for (let i = want; i > Math.max(1, want - 600); i--) {
          if (at(i - 1) <= 0 && at(i) > 0) { start = i; break; }
        }
        for (let i = 0; i < SCOPE_SAMPLES; i++) {
          const f = Math.min(frames - 1, start + i);
          scopeL[i] = at(f);
          scopeR[i] = workletStereo ? out[2 * f + 1] : at(f);
        }
        let pcmRms = 0;
        for (let i = 0; i < pcm.length; i++) pcmRms += pcm[i] * pcm[i];
        pcmRms = Math.sqrt(pcmRms / Math.max(1, pcm.length));
        let sideRms = 0;
        for (let i = 0; i < side.length; i++) sideRms += side[i] * side[i];
        sideRms = Math.sqrt(sideRms / Math.max(1, side.length));
        const blockDb = 20 * Math.log10(Math.max(peak, 1e-6));
        audioPeakHoldDb = Math.max(blockDb, audioPeakHoldDb - AUDIO_HOLD_DECAY_DB);
        let ms = 0;
        for (let i = 0; i < metered.length; i++) ms += metered[i] * metered[i];
        ms /= Math.max(1, metered.length);
        const aVu = 1 - Math.exp(-BLOCK_S / VU_TAU_S);
        audioVuMs = audioVuMs === 0 ? ms : audioVuMs + aVu * (ms - audioVuMs);
        const vuDb = 10 * Math.log10(Math.max(audioVuMs, 1e-12));
        post({
          type: 'wfm-info', stereo: info.stereo,
          peakDevHz: Math.round(info.peakDevHz),
          pilotPct: Math.round(info.pilotRatio * 1000) / 10,
          peakDbFs: vuDb.toFixed(1),
          peakHoldDb: audioPeakHoldDb.toFixed(1),
          rdsLocked: info.rdsLocked,
          rdsPi: info.rdsPi,
          rdsGroups: info.rdsGroups,
          rdsBits: info.rdsBits,
          rdsRefMag: info.rdsRefMag,
          rdsAxisI: info.rdsAxisI,
          rdsAxisQ: info.rdsAxisQ,
          ps: text?.ps ?? '',
          rt: text?.rt ?? '',
          carrierHz: centerHz,
          pilotDevHz: info.pilotDevHz,
          pilotSnrDb: info.pilotSnrDb,
          // DIAG: what the pipeline actually produced this block.
          // DIAG: the side chain's own internals (pilot phasor, its working rate, the block's
          // decimated length). The pane's eight words can say MONO without saying why.
          pilotI: info.pilotI,
          pilotQ: info.pilotQ,
          auxRateHz: info.auxRateHz,
          lastAux: info.lastAux,
          // DIAG: the geometry the pipeline was built with, against the stream's own rate.
          fsIn: params?.fsIn ?? 0,
          ifBw: params?.ifBw ?? 0,
          sideLen: side.length,
          sideRms: Number(sideRms.toFixed(6)),
          pcmRms: Number(pcmRms.toFixed(6)),
          scopeL: scopeL.slice(),
          scopeR: scopeR.slice(),
        });
      }
    }
    // DeepFilterNet3 owns the PCM only while the listener has it selected and it is ready; a
    // failed/slow model has already fallen back, so the block passes through the Wiener chain.
    // NR stays mono by design (a speech reducer has no stereo contract); in that case the block
    // delivers as mono and the worklet splits it, so the two paths never disagree about layout.
    if (nr && nrAlgo === 'dfn' && dfnReady) {
      // NR is mono by design (a speech reducer has no stereo contract); the dfn response is
      // duplicated on delivery so the block keeps the worklet's layout.
      deliverViaDfn(pcm); // async: the dfn worker's 'pcm' response is delivered on its message
    } else {
      deliver(out);
    }
    procMsAccum += performance.now() - procStart;
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
  } else if (msg.type === 'sstv-mode') {
    // The decode window's mode pin: `index >= 0` selects a mode, anything else clears it (the VIS
    // header decides). A protocol without the knob refuses it, which the reply reports.
    const index = Number(msg.index);
    const wanted = Number.isFinite(index) && index >= 0 ? index : null;
    const applied = pipeline?.setModePin(wanted) ?? false;
    post({ type: 'sstv-mode', index: applied ? (wanted ?? -1) : -1 });
  } else if (msg.type === 'enabled') {
    enabled = Boolean(msg.value);
    if (!enabled) {
      pipeline?.free();
      pipeline = null;
      companion?.free();
      companion = null;
      resetDelivery();
    }
    if (!enabled) pushDecoderParams();
    syncPipeline();
    postStats();
  } else if (msg.type === 'configure') {
    const next = msg.params as PipelineParams;
    if (next && Number(next.fsIn) > 0 && Number(next.outRate) > 0) {
      // 20 ms at the output rate: the worklet's own re-prime threshold.
      pcmBlockSamples = Math.max(128, Math.round(Number(next.outRate) * 0.02));
      const previous = params;
      params = next;
      lastConfig = { fsIn: Number(next.fsIn), mode: String(next.mode), ifBw: Number(next.ifBw), at: Date.now() };
      workletStereo = next.mode === 'wfm';
      workletPort?.postMessage({ type: 'buffer-mode', mode: next.mode, stereo: workletStereo });
      if (typeof msg.volume === 'number') volume = msg.volume;
      if (typeof msg.audioEnabled === 'boolean') audioEnabled = msg.audioEnabled;
      if (typeof msg.nr === 'boolean') nr = msg.nr;
      if (typeof msg.nrStrength === 'number') nrStrength = msg.nrStrength;
      if (typeof msg.nrAlgo === 'string') nrAlgo = msg.nrAlgo === 'dfn' ? 'dfn' : 'wiener';
      if (typeof msg.nrAtten === 'number') nrAtten = msg.nrAtten;
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
      } else if (pipeline && pipeline.isDigital && pipeline.mode === next.mode
                 && (next.mode === 'drm' || next.mode === 'drmplus')
                 && (rateChanged || previous?.ifBw !== next.ifBw || previous?.outRate !== next.outRate)) {
        // Selecting DRM+ first uses the *old* backend DDC rate. Rebuild once the 100 kHz
        // channelizer reports its actual rate; otherwise the decoder resamples it as if it
        // were the previous narrow channel for the rest of the session.
        pipeline.reconfigure(next, true);
        post({ type: 'drm-clear' });
        resetDelivery();
        if (!pipeline.ok) {
          pipeline.free();
          pipeline = null;
          post({ type: 'error', message: `no DSP pipeline for mode ${next.mode}` });
        }
      }
      // A digital mode's decoder is rebuilt by its own worker; here only the companion changes.
      syncCompanion(digitalIds.has(next.mode));
      applyListenerControls();
      syncDfn();
      syncCwDecoder();
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
    if (typeof msg.algo === 'string') nrAlgo = msg.algo === 'dfn' ? 'dfn' : 'wiener';
    if (typeof msg.atten === 'number') nrAtten = msg.atten;
    applyListenerControls();
    syncDfn();
    // Live-update the attenuation limit on an already-running stream (syncDfn only rebuilds).
    dfnWorker?.postMessage({ type: 'atten', value: nrAtten });
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
        workletPlaying = status.playing ? 1 : 0;
        workletPrimed = status.primed ? 1 : 0;
        // DIAG: forward gate transitions immediately - the stats cadence (25 blocks) quantizes
        // the bench's retune-gap measurement to ~800 ms otherwise.
        if (workletPlaying !== lastGatePlaying || workletUnderruns !== lastGateUnderruns) {
          lastGatePlaying = workletPlaying;
          lastGateUnderruns = workletUnderruns;
          post({ type: 'gate', playing: workletPlaying, underruns: workletUnderruns,
                 resets: workletRingResets, flushes, delivered: workletReceived,
                 blocks, at: Date.now(),
                 wEnabled: status.enabled, wPrime: status.prime,
                 wFast: status.fastPrime, wFrames: status.framesNow });
        }
        workletReceived = Number(status.received) || 0;
        workletRingResets = Number(status.resets) || 0;
        workletSlipped = Number(status.slipped) || 0;
        workletRatio = Number(status.ratio) || 1;
        const fill = Number(status.available) || 0;
        fillMin = Math.min(fillMin, fill);
        fillMax = Math.max(fillMax, fill);
      };
      workletPort.start();
      workletPort.postMessage({ type: 'buffer-mode', mode: params?.mode, stereo: workletStereo });
      workletPort.postMessage({ type: 'enabled', value: enabled });
    }
  } else if (msg.type === 'dump-iq') {
    dumpIq = { on: Boolean(msg.value), frames: [] };
  } else if (msg.type === 'retune') {
    // The listened channel moved inside the SAME capture (the backend keeps streaming, so no
    // frame carries `seq === 0` and nothing else would tell the kernel). Its stereo state — the
    // pilot phasor, the pilot SNR, the smoothed peak deviation — belongs to the station that was
    // tuned; carried over, a switch away and back reads MONO until a full re-tune, which is what
    // a bench report showed. The level control survives, as on the stream's own retune.
    pipeline?.retune();
    // No ring drop here: the backend's flush carries the one reset. Dropping the ring here as
    // well made the retune re-prime twice (measured ~1 s of silence).
    resetDelivery(false, true);
  } else if (msg.type === 'reset') {
    resetStream();
  } else if (msg.type === 'detach') {
    workletPort = null;
    pendingPcm = [];
    pendingSamples = 0;
  }
};

post({ type: 'ready' });
