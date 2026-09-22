// The FT8 decoder host — a worker of its own, on purpose.
//
// A slot decode is one synchronous WebAssembly call that runs for seconds (measured: about 2.5 s per
// 15 s slot on the bench), and while it runs the thread is gone: any audio produced in the same
// worker stops, the playback ring drains, and the listener hears a gap every slot. That is why the
// decoder lives here, fed a *copy* of the same channelized baseband the audio worker demodulates:
// the two consumers of one baseband in two threads, so a decode can never starve the audio.
//
// The audio worker forwards the decodes it receives from here to the page, so the FT8 readout and the
// decode table keep working without a second socket.
import { decodeFrame } from '../core/frames';
import { dsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';
import { loadPluginManifest } from './registry';

let module: DspModule | null = null;
let pipeline: WasmPipeline | null = null;
let params: PipelineParams | null = null;
let enabled = false;
/// The decoder's own IQ socket.
///
/// It used to be fed by the audio worker, and the audio paid for it: a slot decode blocks this thread
/// for about a second, during which the audio worker's hand-off backed up and the audio it produced
/// jumped (~one discontinuity per decode attempt, measured). Two consumers of one stream belong on two
/// sockets - the backend fans out already - so the audio worker never touches this worker at all.
let ws: WebSocket | null = null;
let wsUrl = '';
let lastSeq = -1;
let dropped = 0;
let pushes = 0;
/// Frames dropped because they queued up behind a decode attempt and went stale.
let skipped = 0;
/// Decode attempts. The decoder owns the windowing now, so the feeder observes an attempt as the
/// decoder's window sliding -- which shows up as its buffered length dropping.
let attempts = 0;
/// Decoder buffered length at the previous frame, used to spot that slide.
let lastBuffered = 0;

function post(msg: Record<string, unknown>): void {
  (self as unknown as { postMessage: (m: unknown) => void }).postMessage(msg);
}

self.onmessage = (event: MessageEvent) => {
  const msg = (event.data || {}) as Record<string, any>;
  if (msg.type === 'init') {
    const wasmUrl = String(msg.wasmUrl || '');
    wsUrl = String(msg.url || '');
    if (!wasmUrl) return;
    // The registry's loader: it caches the manifest the mode helpers read.
    void loadPluginManifest(wasmUrl)
      .then(() => {
        module = dsp();
        post({ type: 'ready' });
        if (wsUrl) connect();
      })
      .catch(() => post({ type: 'error', message: 'dsp.wasm unavailable (FT8)' }));
  } else if (msg.type === 'configure') {
    params = (msg.params as PipelineParams) ?? null;
    enabled = Boolean(msg.enabled);
    build();
  } else if (msg.type === 'socket') {
    // A reconnect (the audio worker watches its own socket; this one does the same).
    if (!ws) connect();
  } else if (msg.type === 'stop') {
    pipeline?.free();
    pipeline = null;
  }
};

/** Own IQ socket: the baseband frames reach this worker directly, never through the audio thread. */
function connect(): void {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  try {
    ws = new WebSocket(wsUrl);
  } catch {
    scheduleReconnect();
    return;
  }
  ws.binaryType = 'arraybuffer';
  ws.onmessage = (event: MessageEvent) => {
    const data = event.data;
    if (!(data instanceof ArrayBuffer)) return;
    const frame = decodeFrame(data);
    if (frame === null || frame.kind !== 'baseband') return;
    onBaseband(frame);
  };
  ws.onclose = () => { ws = null; scheduleReconnect(); };
  ws.onerror = () => { ws?.close(); };
}

function scheduleReconnect(): void {
  setTimeout(connect, 1000);
}

function onBaseband(frame: { seq: number; iq: Float32Array; centerHz: number }): void {
  if (frame.seq === 0) {
    lastSeq = -1;
    pipeline?.reset();
    if (frame.iq.length === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;
    pipeline?.reset();                    // a gap tears a slot: start the next one clean
  }
  if (frame.seq !== 0) lastSeq = frame.seq;
  if (!enabled || !pipeline || frame.iq.length === 0) return;
  // The decoder owns the windowing: it accumulates a *sliding* window of `hop + burst`, searches all
  // of it, then advances one hop and keeps the overlap. So the feeder just hands over the stream.
  //
  // Feeding a per-slot block and clearing in between (what this used to do) made decoding depend on
  // the feeder's own alignment with the 15 s schedule: the window began at the feeder's boundary,
  // which is anchored on frame *arrival* while the samples were captured earlier, so it started late
  // in signal-time and cut the head of every burst. A burst at a seam was simply in no window.
  const reports = pipeline.push(frame.iq);
  for (const report of reports) {
    post({ type: 'ft8', ...report, centerHz: frame.centerHz });
  }
  const buffered = pipeline.buffered();
  if (buffered < lastBuffered) attempts += 1;
  lastBuffered = buffered;
  pushes += 1;
  // The stats are throttled, and that is not cosmetic: the audio worker relays them to the page, and
  // one per frame is ~120 messages a second. That traffic - which exists only in a digital mode,
  // because only then is this worker running - saturated the page's message plumbing enough that the
  // PCM reaching the AudioWorklet arrived in bursts: measured in the live page, the playback ring swung
  // between 0 and its 897 ms ceiling and dropped 30308 samples to the ceiling trim, which the listener
  // hears as a periodic stutter. Four a second says the same thing.
  if (pushes % 30 === 0) {
    post({
      type: 'ft8-stats',
      pushes,
      buffered: pipeline.buffered(),
      decodes: pipeline.count(),
      dropped,
      skipped,
      attempts,
    });
  }
}

/** (Re)build the decoder for the current parameters. */
function build(): void {
  if (!module || !enabled || !params) {
    pipeline?.free();
    pipeline = null;
    return;
  }
  if (pipeline && pipeline.mode === params.mode && pipeline.isDigital && pipeline.ok) {
    return;
  }
  pipeline?.free();
  pipeline = new WasmPipeline(module, params, true);
  if (!pipeline.ok) {
    pipeline.free();
    pipeline = null;
    post({ type: 'error', message: `no FT8 decoder for mode ${params.mode}` });
  }
}

post({ type: 'ready' });
