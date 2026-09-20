// SDR IQ ingress worker — the DSP host.
//
// Owns a dedicated `?iq=1` WebSocket, decodes the IQDF frames the backend publishes at the DDC
// input rate, runs the Rust/WASM receive pipeline over them (DDC -> analog demod -> audio chain)
// and hands the resulting PCM to the AudioWorklet's port, which the main thread transfers here.
// Neither reception, DSP nor playback delivery touches the main thread, so a busy render loop can
// no longer starve the audio.
//
// If the pipeline cannot be created (an unknown mode, a module that failed to load) the worker says
// so in its stats and the main thread keeps the Python audio path: the DSP moving to the browser
// must never be the reason there is no audio.
import { decodeFrame, type IqFrame } from '../core/frames';
import { loadDsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';

let ws: WebSocket | null = null;
let wsUrl = '';
let wasmUrl = '';
let module: DspModule | null = null;
let pipeline: WasmPipeline | null = null;
let params: PipelineParams | null = null;
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
  });
}

/** Create or rebuild the pipeline for the current parameters. */
function syncPipeline(): void {
  if (!enabled || !params || !module) return;
  if (pipeline && pipeline.mode === params.mode) {
    pipeline.setVolume(volume);
    pipeline.setAudioEnabled(audioEnabled);
    return;
  }
  const next = new WasmPipeline(module, params);
  if (!next.ok) {
    // Declared but not runnable: report it instead of pretending it works.
    next.free();
    pipeline = null;
    post({ type: 'error', message: `no DSP pipeline for mode ${params.mode}` });
    postStats();
    return;
  }
  pipeline?.free();
  pipeline = next;
  pipeline.setVolume(volume);
  pipeline.setAudioEnabled(audioEnabled);
  post({ type: 'ready', mode: params.mode });
  postStats();
}

function deliver(pcm: Float32Array): void {
  if (pcm.length === 0) return;
  pcmFrames++;
  pcmSamples += pcm.length;
  let sum = 0;
  for (let index = 0; index < pcm.length; index++) sum += pcm[index] * pcm[index];
  rms = Math.sqrt(sum / pcm.length);
  if (workletPort) {
    // Transferred, so the audio thread never waits for a copy.
    workletPort.postMessage({ type: 'samples', samples: pcm }, [pcm.buffer]);
  }
}

function resetStream(): void {
  lastSeq = -1;
  flushes++;
  pipeline?.free();
  pipeline = null;
  syncPipeline();
}

function onIq(frame: IqFrame): void {
  if (frame.seq === 0) {
    resetStream();
    if (frame.samples === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;      // a gap the client never saw (overrun on the wire)
  }
  if (frame.seq !== 0) lastSeq = frame.seq;
  rate = frame.rate;
  centerHz = frame.centerHz;
  blocks++;
  samples += frame.samples;
  if (pipeline) deliver(pipeline.process(frame.iq));
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
    if (frame === null || frame.kind !== 'iq') return;
    onIq(frame);
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
    if (!enabled) pipeline?.free();
    pipeline = enabled ? pipeline : null;
    syncPipeline();
    postStats();
  } else if (msg.type === 'configure') {
    const next = msg.params as PipelineParams;
    if (next && Number(next.fsIn) > 0) {
      const geometryChanged =
        !params ||
        params.mode !== next.mode ||
        params.ifBw !== next.ifBw ||
        params.decimate !== next.decimate ||
        params.outRate !== next.outRate ||
        params.pitch !== next.pitch;
      params = next;
      if (pipeline && geometryChanged) pipeline.reconfigure(next);
      else if (pipeline && params) pipeline.retune(next.offsetHz);
      if (typeof msg.volume === 'number') volume = msg.volume;
      if (typeof msg.audioEnabled === 'boolean') audioEnabled = msg.audioEnabled;
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
      workletPort.start();
      workletPort.postMessage({ type: 'enabled', value: enabled });
    }
  } else if (msg.type === 'reset') {
    resetStream();
  } else if (msg.type === 'detach') {
    workletPort = null;
  }
};

post({ type: 'ready' });
