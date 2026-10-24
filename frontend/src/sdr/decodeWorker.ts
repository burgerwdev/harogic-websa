import { DecodeSequence, MAX_DECODE_BYTES, type DecodeEnvelope } from './decodeProtocol';

interface DecoderWorkerHost {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<{ type: string; url?: string; message?: string }>) => void) | null;
}
// WebWorker and Window expose the same message methods; this module runs only as a worker.
const host = self as unknown as DecoderWorkerHost;
let socket: WebSocket | null = null;
let enabled = false;
let url = '';
let retry: number | null = null;
let inFlight = 0;
let inFlightBytes = 0;
let queuedBytes = 0;
const queue: { envelope: DecodeEnvelope; bytes: number }[] = [];
const MAX_QUEUED_EVENTS = 32;
const MAX_QUEUED_BYTES = 4 * 1024 * 1024;
function fail(message: string): void {
  enabled = false;
  queue.length = 0;
  queuedBytes = 0;
  const previous = socket;
  socket = null;
  // Browser clients may send 1000 or application codes, never protocol code 1002.
  // Publish the original validation error before closing its transport.
  host.postMessage({ type: 'error', message });
  previous?.close(4002, 'Decoded-output stream rejected');
}
function deliver(): void {
  if (inFlight || queue.length === 0) return;
  const items = queue.splice(0);
  inFlight = items.length;
  inFlightBytes = queuedBytes;
  queuedBytes = 0;
  host.postMessage({ type: 'frames', envelopes: items.map((item) => item.envelope) });
}
function connect(): void {
  if (!enabled || socket) return;
  const connection = new WebSocket(url);
  socket = connection;
  connection.binaryType = 'arraybuffer';
  const sequence = new DecodeSequence();
  connection.onopen = () => host.postMessage({ type: 'connected' });
  connection.onmessage = (event: MessageEvent<unknown>) => {
    if (!enabled || socket !== connection) return;
    try {
      if (!(event.data instanceof ArrayBuffer) || event.data.byteLength - 8 > MAX_DECODE_BYTES) throw new Error('Expected bounded binary SDE2');
      const envelope = sequence.accept(event.data);
      if (queue.length + inFlight >= MAX_QUEUED_EVENTS || queuedBytes + inFlightBytes + event.data.byteLength > MAX_QUEUED_BYTES) {
        throw new Error('Slow decoded-output consumer: queue exceeded');
      }
      queuedBytes += event.data.byteLength;
      queue.push({ envelope, bytes: event.data.byteLength });
      deliver();
    } catch (error) { fail(error instanceof Error ? error.message : String(error)); }
  };
  connection.onerror = () => host.postMessage({ type: 'error', message: 'Decoded-output connection failed' });
  connection.onclose = (event) => {
    if (socket !== connection) return;
    socket = null;
    if (!enabled) return;
    if (event.code === 1013) { fail(`Slow decoded-output consumer: ${event.reason}`); return; }
    if (event.code === 1011 && event.reason === 'receiver failed') {
      fail('Decoded-output receiver failed');
      return;
    }
    host.postMessage({ type: 'closed', message: `Decoded-output stream closed (${event.code}): ${event.reason}` });
    // FIFO delivery preserves old tails before the next stream boundary.
    retry = self.setTimeout(() => { retry = null; connect(); }, 2000);
  };
}
host.onmessage = (event: MessageEvent<{ type: string; url?: string; message?: string }>) => {
  const message = event.data;
  if (message.type === 'start') {
    enabled = true;
    url = message.url ?? '';
    connect();
  } else if (message.type === 'ack') {
    inFlight = 0;
    inFlightBytes = 0;
    deliver();
  } else if (message.type === 'error') {
    fail(message.message ?? 'Invalid decoded output');
  } else if (message.type === 'stop') {
    enabled = false;
    if (retry !== null) { self.clearTimeout(retry); retry = null; }
    const previous = socket;
    socket = null;
    previous?.close();
    queue.length = 0;
    queuedBytes = 0;
  }
};
