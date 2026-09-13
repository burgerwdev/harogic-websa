// SDR audio ingress worker.
//
// Owns a dedicated `?audio=1` WebSocket, parses the AUDF frames, resamples them and feeds
// the AudioWorklet's MessagePort directly (the main thread transfers that port here). The
// point is that neither reception nor playback delivery happens on the main thread, so a
// busy render loop can no longer starve the audio ring - the failure mode where the ring
// drained (underruns) whenever the page stalled for a few hundred ms.
//
// Browsers without AudioWorklet keep the ScriptProcessor output, which lives on the main
// thread; there the worker posts the resampled buffers back for the legacy ring instead.
import { StreamingPcm16Resampler } from './sdrResampler';
import { decodeFrame } from '../core/frames';

let ws: WebSocket | null = null;
let workletPort: MessagePort | null = null;
let resampler = new StreamingPcm16Resampler();
let wsUrl = '';
let targetRate = 48000;
let sourceRate = 48000;
let enabled = false;
let muted = false;
let frames = 0;
let buffered = 0;
let underruns = 0;
let rms = 0;
let reconnectTimer: number | null = null;
let reconnectDelay = 500;

function post(msg: Record<string, unknown>, transfer?: Transferable[]): void {
  // WorkerGlobalScope.postMessage(message, transferList)
  (self as unknown as { postMessage: (m: unknown, t?: Transferable[]) => void })
    .postMessage(msg, transfer || []);
}

function postStats(): void {
  post({ type: 'stats', frames, bufferedSamples: buffered, underruns, rms });
}

function resetPipeline(): void {
  resampler.reset();
  if (workletPort) workletPort.postMessage({ type: 'reset' });
  else post({ type: 'reset' });
}

function deliver(samples: Float32Array): void {
  if (workletPort) workletPort.postMessage({ type: 'samples', samples }, [samples.buffer]);
  else post({ type: 'samples', samples }, [samples.buffer]);
}

function onAudio(buffer: ArrayBuffer, offset: number, samples: number, rate: number, reset: boolean): void {
  frames++;
  if (samples * 2 + offset > buffer.byteLength) return;
  // Level of the incoming block, for the canvas diagnostic (not used for playback).
  if (samples > 0) {
    const view = new DataView(buffer, offset, samples * 2);
    let sum = 0;
    for (let i = 0; i < samples; i++) {
      const s = view.getInt16(i * 2, true) / 32768;
      sum += s * s;
    }
    rms = Math.sqrt(sum / samples);
  }
  if (reset) resetPipeline();
  if (!enabled || muted || samples === 0) {
    if (frames % 25 === 0) postStats();
    return;
  }
  if (rate > 0) sourceRate = rate;
  const out = resampler.process(buffer, offset, samples, sourceRate, targetRate);
  if (out.length) deliver(out);
  if (frames % 25 === 0) postStats();
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
    if (frame === null || frame.kind !== 'audio') return;
    onAudio(d, frame.pcm.byteOffset, frame.samples, frame.rate, frame.seq === 0);
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
    targetRate = Number(msg.targetRate) || 48000;
    if (msg.port) {
      workletPort = msg.port as MessagePort;
      workletPort.onmessage = (e: MessageEvent) => {
        const d = (e.data || {}) as Record<string, any>;
        if (d.type === 'status') {
          buffered = Number(d.available) || 0;
          underruns = Number(d.underruns) || 0;
          postStats();
        }
      };
      workletPort.start();
      workletPort.postMessage({ type: 'enabled', value: enabled });
    }
    if (wsUrl) connect();
  } else if (msg.type === 'enabled') {
    enabled = Boolean(msg.value);
    workletPort?.postMessage({ type: 'enabled', value: enabled });
  } else if (msg.type === 'mute') {
    muted = Boolean(msg.value);
  } else if (msg.type === 'reset') {
    resetPipeline();
  } else if (msg.type === 'detach') {
    // The worklet failed after the port was transferred: fall back to main-thread delivery.
    workletPort = null;
  }
};

post({ type: 'ready' });
