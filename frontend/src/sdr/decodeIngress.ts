import { DecodeSequence, MAX_DECODE_BYTES, type DecodeEnvelope } from './decodeProtocol';
import { receiverEndReason, StreamRetry } from './streamRetry';
import { MAX_QUEUED_EVENTS, MAX_QUEUED_BYTES } from './streamLimits';

export interface DecoderWorkerHost {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  close?(): void;
  onmessage: ((event: MessageEvent<{ type: string; url?: string; message?: string }>) => void) | null;
}
export function createDecodeIngress(host: DecoderWorkerHost): void {
  let socket: WebSocket | null = null;
  let enabled = false;
  let url = '';
  let retry: number | null = null;
  let pendingEnd: string | null = null;
  let recovery = new StreamRetry();
  let inFlight = 0;
  let inFlightBytes = 0;
  let queuedBytes = 0;
  // Progress/scopes can arrive faster than a useful UI cadence. Preserve every
  // validated event and ACK, but cross the worker boundary at most ten times a
  // second. Source boundaries and the accepted FIFO tail are delivered promptly.
  const DELIVERY_INTERVAL_MS = 100;
  let deliveryTimer: ReturnType<typeof setTimeout> | null = null;
  let lastDelivery = -Infinity;
  const queue: { envelope: DecodeEnvelope; bytes: number; connection: WebSocket }[] = [];
  const applied = new Map<string, { envelope: DecodeEnvelope; connection: WebSocket }>();
  const snapshotAck = new WeakMap<WebSocket, boolean>();
  function clearDelivery(): void {
    if (deliveryTimer !== null) clearTimeout(deliveryTimer);
    deliveryTimer = null; lastDelivery = -Infinity;
    inFlight = 0;
    inFlightBytes = 0;
    queue.length = 0;
    queuedBytes = 0;
    applied.clear();
    pendingEnd = null;
  }
  function finishEnd(): void {
    if (pendingEnd === null || inFlight || queue.length) return;
    const reason = pendingEnd;
    clearDelivery();
    host.postMessage({ type: 'ended', reason });
  }
  function fail(message: string): void {
    enabled = false;
    if (retry !== null) { self.clearTimeout(retry); retry = null; }
    clearDelivery();
    const previous = socket;
    socket = null;
    // Browser clients may send 1000 or application codes, never protocol code 1002.
    // Publish the original validation error before closing its transport.
    host.postMessage({ type: 'error', message });
    previous?.close(4002, 'Decoded-output stream rejected');
  }
  function deliver(immediate = false): void {
    if (inFlight || queue.length === 0) return;
    const remaining = DELIVERY_INTERVAL_MS - (performance.now() - lastDelivery);
    const boundary = pendingEnd !== null || queue.some(item => item.envelope.event.type === 'start' || item.envelope.event.type === 'processing-end');
    if (!immediate && !boundary && remaining > 0) {
      deliveryTimer ??= setTimeout(() => { deliveryTimer = null; deliver(true); }, remaining);
      return;
    }
    if (deliveryTimer !== null) clearTimeout(deliveryTimer);
    deliveryTimer = null; lastDelivery = performance.now();
    const items = queue.splice(0);
    inFlight = items.length;
    inFlightBytes = queuedBytes;
    queuedBytes = 0;
    for (const item of items) {
      const { channel, channel_id, epoch } = item.envelope;
      applied.set(`${channel}:${channel_id}:${epoch}`, item);
    }
    host.postMessage({ type: 'frames', envelopes: items.map((item) => item.envelope) });
  }
  function connect(): void {
    if (!enabled || socket) return;
    const endpoint = new URL(url);
    endpoint.searchParams.set('ack', '1');
    const connection = new WebSocket(endpoint.href);
    socket = connection;
    connection.binaryType = 'arraybuffer';
    const sequence = new DecodeSequence();
    connection.onopen = () => { if (enabled && socket === connection) host.postMessage({ type: 'connected' }); };
    connection.onmessage = (event: MessageEvent<unknown>) => {
      if (!enabled || socket !== connection) return;
      try {
        if (!(event.data instanceof ArrayBuffer) || event.data.byteLength - 8 > MAX_DECODE_BYTES) throw new Error('Expected bounded binary SDE2');
        const envelope = sequence.accept(event.data);
        if (envelope.seq > 0) recovery.progress();
        if (envelope.seq === 0 && envelope.event.type === 'start' && envelope.event.snapshotAck === true) {
          snapshotAck.set(connection, true);
        }
        if (queue.length + inFlight >= MAX_QUEUED_EVENTS || queuedBytes + inFlightBytes + event.data.byteLength > MAX_QUEUED_BYTES) {
          throw new Error('Slow decoded-output consumer: queue exceeded');
        }
        queuedBytes += event.data.byteLength;
        queue.push({ envelope, bytes: event.data.byteLength, connection });
        deliver();
      } catch (error) { fail(error instanceof Error ? error.message : String(error)); }
    };
    // Browser onerror precedes onclose and has no useful failure classification.
    // Let onclose schedule a new subscription instead of killing this owner.
    connection.onerror = () => {};
    connection.onclose = (event) => {
      if (socket !== connection) return;
      socket = null;
      if (!enabled) return;
      const reason = receiverEndReason(event.code, event.reason);
      if (reason !== null) {
        enabled = false;
        if (retry !== null) { self.clearTimeout(retry); retry = null; }
        // Server close follows its FIFO tail. Already validated local deliveries
        // still belong to that source and must reach the host before it ends.
        pendingEnd = reason;
        deliver(true);
        finishEnd();
        return;
      }
      const delay = recovery.delay(event.code, event.reason);
      if (delay !== null) {
        host.postMessage({ type: 'closed', message: `Decoded-output stream interrupted (${event.code}): ${event.reason}; reconnecting` });
        retry = self.setTimeout(() => { retry = null; connect(); }, delay);
        return;
      }
      if (event.code === 1013) { fail(`Slow decoded-output consumer: ${event.reason}`); return; }
      if (event.code === 1011 && event.reason === 'receiver failed') {
        fail('Decoded-output receiver failed');
        return;
      }
      if (event.code === 1011 && event.reason === 'subscriber source stream invalid') {
        fail('Decoded-output source stream invalid');
        return;
      }
      fail(`Decoded-output stream closed (${event.code}): ${event.reason}`);
    };
  }
  host.onmessage = (event: MessageEvent<{ type: string; url?: string; message?: string }>) => {
    const message = event.data;
    if (message.type === 'start') {
      if (enabled) return;
      clearDelivery();
      recovery = new StreamRetry();
      enabled = true;
      url = message.url ?? '';
      connect();
    } else if (message.type === 'ack') {
      if (!inFlight) return;
      const acknowledgements = [...applied.values()];
      applied.clear();
      inFlight = 0;
      inFlightBytes = 0;
      try {
        for (const { envelope, connection } of acknowledgements) {
          if (enabled && socket === connection && snapshotAck.get(connection) === true) connection.send(JSON.stringify({ type: 'ack',
            channel: envelope.channel, channel_id: envelope.channel_id, epoch: envelope.epoch, seq: envelope.seq }));
        }
      } catch (error) { fail(error instanceof Error ? error.message : String(error)); return; }
      deliver();
      finishEnd();
    } else if (message.type === 'error') {
      fail(message.message ?? 'Invalid decoded output');
    } else if (message.type === 'stop' || message.type === 'dispose') {
      enabled = false;
      if (retry !== null) { self.clearTimeout(retry); retry = null; }
      clearDelivery();
      const previous = socket;
      socket = null;
      previous?.close();
      if (message.type === 'dispose') { host.onmessage = null; host.close?.(); }
    }
  };

}
