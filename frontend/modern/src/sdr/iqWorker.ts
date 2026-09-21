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
import { loadDsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';
import { readPluginManifest } from './registry';

let ws: WebSocket | null = null;
let wsUrl = '';
let wasmUrl = '';
let module: DspModule | null = null;
/// The demodulator: one handle for both paths (analog PCM, digital text).
let pipeline: WasmPipeline | null = null;
let params: PipelineParams | null = null;
let digital = false;
let ft8Messages = 0;
/// What the worklet reports about its own ring (relayed to the page's diagnostics).
let workletAvailable = 0;
let workletUnderruns = 0;
let workletReceived = 0;
/// The last failure seen while handing PCM to the worklet ('' when it is fine).
let workletError = '';
let workletRingResets = 0;
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
/// Ids the module declares as digital protocols; the registry is the single source of truth, so a
/// new protocol becomes reachable here without a second list in the worker.
let digitalIds = new Set<string>();
let audioEnabled = true;
let volume = 1;
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

function post(msg: Record<string, unknown>, transfer?: Transferable[]): void {
  // WorkerGlobalScope.postMessage(message, transferList)
  (self as unknown as { postMessage: (m: unknown, t?: Transferable[]) => void })
    .postMessage(msg, transfer || []);
}

function postStats(): void {
  post({
    type: 'stats', enabled, blocks, samples, rate, centerHz, dropped, flushes,
    pcmFrames, pcmSamples, rms, pipeline: pipeline?.ok ?? false, mode: params?.mode ?? '',
    ft8Messages, digitalPushes, digitalBuffered, digitalResets,
    worklet: workletPort ? 1 : 0, workletAvailable, workletUnderruns, workletReceived,
    workletError, workletRingResets, pcmPending: pendingSamples, deliveredSamples,
  });
}

/** Create or rebuild the pipeline for the current parameters. */
function syncPipeline(): void {
  if (!enabled || !params || !module) return;
  const isDigital = digitalIds.has(params.mode);
  if (pipeline && pipeline.mode === params.mode && pipeline.isDigital === isDigital) {
    pipeline.setVolume(volume);
    pipeline.setAudioEnabled(audioEnabled);
    return;
  }
  if (pipeline) {
    pipeline.reconfigure(params, isDigital);
  } else {
    pipeline = new WasmPipeline(module, params, isDigital);
  }
  if (!pipeline.ok) {
    // Declared but not runnable: report it instead of pretending it works.
    pipeline.free();
    pipeline = null;
    post({ type: 'error', message: `no DSP pipeline for mode ${params.mode}` });
    postStats();
    return;
  }
  digital = isDigital;
  pipeline.setVolume(volume);
  pipeline.setAudioEnabled(audioEnabled);
  post({ type: 'ready', mode: params.mode });
  postStats();
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
  if (pipeline && digital) {
    digitalPushes += 1;
    const message = pipeline.push(frame.iq);
    digitalBuffered = pipeline.buffered();
    if (message) {
      ft8Messages = message.count;
      post({ type: 'ft8', ...message, count: ft8Messages });
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
    if (wasmUrl) {
      void loadDsp(wasmUrl)
        .then((loaded) => {
          module = loaded;
          // Which ids are protocols (as opposed to audio demodulators) comes from the module's own
          // plugin manifest: the worker does not carry a list of its own.
          digitalIds = new Set(
            readPluginManifest(loaded)
              .filter((plugin) => plugin.kind === 'digital')
              .map((plugin) => plugin.id),
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
      const geometryChanged =
        !previous ||
        previous.mode !== next.mode ||
        previous.ifBw !== next.ifBw ||
        previous.fsIn !== next.fsIn ||
        previous.outRate !== next.outRate ||
        previous.pitch !== next.pitch;
      if (pipeline && geometryChanged) {
        // The baseband geometry changed: the demodulator's filters describe another channel.
        pipeline.reconfigure(next, digitalIds.has(next.mode));
        if (!pipeline.ok) {
          pipeline.free();
          pipeline = null;
          post({ type: 'error', message: `no DSP pipeline for mode ${next.mode}` });
        }
      }
      pipeline?.setVolume(volume);
      pipeline?.setAudioEnabled(audioEnabled);
      syncPipeline();
    }
  } else if (msg.type === 'volume') {
    volume = Number(msg.value) || 0;
    pipeline?.setVolume(volume);
  } else if (msg.type === 'audio') {
    audioEnabled = Boolean(msg.value);
    pipeline?.setAudioEnabled(audioEnabled);
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
