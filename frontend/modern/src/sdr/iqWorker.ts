// SDR IQ ingress worker — the DSP host.
//
// Owns a dedicated `?iq=1` WebSocket and decodes the IQDF frames the backend publishes at the
// DDC input rate. The real-time DSP (DDC, analog/digital demodulators, audio enhancement) runs
// in the Rust/WASM module loaded from this worker; keeping the socket here is what keeps the
// DSP input flowing while the main thread is busy rendering.
//
// This module owns transport, framing and diagnostics only. The DSP stages are added on top of
// `deliver()` in the following steps, so the worker contract (stats, flush, reconnect) is
// already the one the pipeline will use.
import { decodeFrame, type IqFrame } from '../core/frames';

let ws: WebSocket | null = null;
let wsUrl = '';
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
let dropped = 0;
let flushes = 0;
let lastSeq = -1;
let reconnectTimer: number | null = null;
let reconnectDelay = 500;

function post(msg: Record<string, unknown>, transfer?: Transferable[]): void {
  // WorkerGlobalScope.postMessage(message, transferList)
  (self as unknown as { postMessage: (m: unknown, t?: Transferable[]) => void })
    .postMessage(msg, transfer || []);
}

function postStats(): void {
  post({ type: 'stats', enabled, blocks, samples, rate, centerHz, dropped, flushes });
}

/** A block whose `seq` is 0 starts a new stream: everything queued describes another centre. */
function resetStream(): void {
  lastSeq = -1;
  flushes++;
}

function onIq(frame: IqFrame): void {
  if (frame.seq === 0) {
    resetStream();
    // A flush frame carries no payload; it only marks the discontinuity.
    if (frame.samples === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;      // a gap the client never saw (overrun on the wire)
  }
  if (frame.seq !== 0) lastSeq = frame.seq;
  rate = frame.rate;
  centerHz = frame.centerHz;
  blocks++;
  samples += frame.samples;
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
    if (wsUrl) connect();
  } else if (msg.type === 'enabled') {
    enabled = Boolean(msg.value);
    postStats();
  } else if (msg.type === 'reset') {
    resetStream();
  }
};

post({ type: 'ready' });
