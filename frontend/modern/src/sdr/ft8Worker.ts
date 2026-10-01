// The FT8 stream worker — the socket reader half of the decoder.
//
// A slot decode is one synchronous WebAssembly call that runs for seconds (measured: ~2.8 s per
// 15 s slot on the bench), and this worker used to run that call itself — with its WebSocket on
// the same thread. While the decode ran, nobody read the socket; the backend's per-client FIFO
// (a bound of 0.4 s, `IQ_LIMIT`) overran and dropped frames, and every decode attempt therefore
// came back to a torn stream: a sequence gap, a window reset, and the slot the decoder had just
// searched was thrown away unreported. Measured live (PlutoSDR transmitting every slot, SAN-90
// receiving): 8 of ~18 slots decoded, `resets=10` against `attempts=5` — the "it only decodes
// occasionally" signature.
//
// So the decoder is two workers now, and this is the cheap one: it owns the WebSocket, decodes
// the frame headers, and hands the sample payload to the decode worker over a MessagePort —
// transferred, not copied (a baseband block is ~36 kB at the DDC rate). The socket is drained
// continuously no matter how long a decode takes; the backlog queues in the decode worker's
// message loop and is consumed after the decode (decode duty is well under real time, so the
// queue drains inside the next slot).
import { decodeFrame } from '../core/frames';

/// The decoder's port (messages: 'baseband' in, 'ready'/'ft8'/'ft8-stats'/'error' out).
let port: MessagePort | null = null;

function post(msg: Record<string, unknown>): void {
  (self as unknown as { postMessage: (m: unknown) => void }).postMessage(msg);
}

self.onmessage = (event: MessageEvent) => {
  const msg = (event.data || {}) as Record<string, any>;
  if (msg.type === 'init') {
    port = (msg.port as MessagePort) ?? null;
    if (port) port.onmessage = (fromDecoder: MessageEvent) => post(fromDecoder.data);
    if (msg.url) connect(String(msg.url));
  } else if (msg.type === 'socket') {
    // A reconnect (the audio worker watches its own socket; this one does the same).
    if (!ws) connect(lastUrl);
  }
};

let ws: WebSocket | null = null;
let lastUrl = '';

/** Own IQ socket: the baseband frames reach this worker directly, never through the audio thread. */
function connect(url: string): void {
  if (!url) return;
  lastUrl = url;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  try {
    ws = new WebSocket(url);
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
    // The frame's buffer belongs to this message alone: transfer it (zero copy) rather than
    // cloning ~36 kB per block. The header bytes ride along; the decoder reads the view only.
    if (port) {
      try {
        port.postMessage(
          { type: 'baseband', seq: frame.seq, iq: frame.iq, centerHz: frame.centerHz },
          [frame.iq.buffer],
        );
      } catch {
        port.postMessage({ type: 'baseband', seq: frame.seq, iq: frame.iq.slice(), centerHz: frame.centerHz });
      }
    }
  };
  ws.onclose = () => { ws = null; scheduleReconnect(); };
  ws.onerror = () => { ws?.close(); };
}

function scheduleReconnect(): void {
  setTimeout(() => connect(lastUrl), 1000);
}
// No 'ready' here: that message means "the decoder's WASM is loaded" and belongs to the decode
// worker (relayed through this one); the socket starts draining on 'init' regardless.
