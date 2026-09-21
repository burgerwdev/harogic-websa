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
/// How old a queued frame may be before it is useless: a slot, past which the decoder's buffer has
/// moved on anyway.
const MAX_FRAME_AGE_MS = 16_000;

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
  } else if (msg.type === 'frame') {
    if (!enabled || !pipeline || !(msg.iq instanceof Float32Array)) return;
    // (Kept for a caller that feeds frames directly; the socket path above is the product one.)
    // A decode attempt blocks this thread for seconds (the band search plus the LDPC stage), during
    // which the audio worker keeps sending the live stream. Feeding that backlog would only make the
    // decoder fall further behind - it would fill a slot again and start another attempt immediately -
    // and the decoder is a live listener, not an archive: audio older than a slot is dropped.
    const at = Number(msg.at) || 0;
    if (at > 0 && Date.now() - at > MAX_FRAME_AGE_MS) {
      skipped += 1;
      if (skipped % 50 === 1) post({ type: 'ft8-stats', pushes, buffered: pipeline.buffered(), decodes: pipeline.count(), skipped });
      return;
    }
    pushes += 1;
    const decoded = pipeline.push(msg.iq as Float32Array);
    post({
      type: 'ft8-stats',
      pushes: pushes,
      buffered: pipeline.buffered(),
      decodes: pipeline.count(),
      skipped,
    });
    if (decoded) post({ type: 'ft8', ...decoded, centerHz: msg.centerHz, count: decoded.count });
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
  // No age/skip logic: with its own socket this worker receives the live stream at its own pace, and
  // the buffer it hands the decoder is contiguous (which a slot needs). A decode that takes a second
  // simply delays the next attempt, exactly as the mode's cadence intends.
  const decoded = pipeline.push(frame.iq);
  post({
    type: 'ft8-stats',
    pushes: pushes,
    buffered: pipeline.buffered(),
    decodes: pipeline.count(),
    dropped,
  });
  if (decoded) post({ type: 'ft8', ...decoded, centerHz: frame.centerHz, count: decoded.count });
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
