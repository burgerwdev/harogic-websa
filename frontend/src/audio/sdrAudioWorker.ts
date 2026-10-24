// One worker owns one backend audio subscription and its playback port.
import { StreamingFloatResampler } from './sdrResampler';

interface Fraction { num: bigint; den: bigint }
interface Identity {
  channel: number; epoch: number; rate: number; inputRate: number;
  tuned: number; delay: number; channels: number; flags: number;
}
let socket: WebSocket | null = null;
let port: MessagePort | null = null;
let url = '';
let targetRate = 48000;
let enabled = false;
let muted = false;
let failed = false;
let identity: Identity | null = null;
let identityHighWater: { epoch: number; channel: number } | null = null;
let playbackMode = 'default';
let receiverMode = 'default';
let modeBoundaryPending = false;
let sequence = 0;
let audioIndex = 0n;
let source: Fraction = { num: 0n, den: 1n };
let sourceStep: Fraction = { num: 0n, den: 1n };
let previousSource: Fraction = { num: 0n, den: 1n };
let pending = 0;
let generation = 0;
let nextDelivery = 0;
const deliveries = new Map<number, number>();
const queuedAudio: { samples: Float32Array; channels: number; offset: number }[] = [];
let flowControl = false;
let sentFrames = 0;
let sinkLimit = 0;
const resampler = new StreamingFloatResampler();
let packets = 0;
let rms = 0;
const MAX_QUEUE_SECONDS = 3;
function post(message: Record<string, unknown>, transfer: Transferable[] = []): void {
  // DOM declares the Window overload; workers accept a transfer list as the second argument.
  const scope = self as unknown as { postMessage: (m: unknown, t: Transferable[]) => void };
  scope.postMessage(message, transfer);
}
function output(message: Record<string, unknown>, transfer: Transferable[] = []): void {
  if (port) port.postMessage(message, transfer);
  else post(message, transfer);
}
function gcd(a: bigint, b: bigint): bigint {
  while (b !== 0n) { const remainder = a % b; a = b; b = remainder; }
  return a;
}
function fraction(num: bigint, den: bigint): Fraction {
  const divisor = gcd(num, den);
  return { num: num / divisor, den: den / divisor };
}
// Preserve the exact binary64 rate rather than rounding source positions to Number.
function rateFraction(value: number): Fraction {
  const bits = new DataView(new ArrayBuffer(8));
  bits.setFloat64(0, value, false);
  const raw = bits.getBigUint64(0, false);
  const exponent = Number((raw >> 52n) & 2047n);
  const mantissa = (raw & ((1n << 52n) - 1n)) + (exponent === 0 ? 0n : 1n << 52n);
  const shift = exponent === 0 ? -1074 : exponent - 1075;
  return shift >= 0 ? fraction(mantissa << BigInt(shift), 1n) : fraction(mantissa, 1n << BigInt(-shift));
}
function flush(): void {
  resampler.reset();
  pending = 0;
  deliveries.clear();
  queuedAudio.length = 0;
  sentFrames = 0;
  sinkLimit = 0;
  generation++;
  output({ type: 'reset', generation, playbackMode, stereo: identity?.channels === 2 });
}
function fail(reason: string): void {
  if (failed) return;
  failed = true;
  const previous = socket;
  socket = null;
  previous?.close(1000, 'Audio stream rejected');
  flush();
  output({ type: 'enabled', value: false });
  post({ type: 'error', message: reason });
}
function acceptStatus(data: Record<string, unknown>): void {
  if (data.type === 'credit') {
    if (data.generation === generation && typeof data.limit === 'number' && Number.isSafeInteger(data.limit) && data.limit >= 0) {
      sinkLimit = data.limit;
      drainAudio();
    }
    return;
  }
  if (data.type === 'accepted' && data.generation === generation && typeof data.delivery === 'number') {
    const count = deliveries.get(data.delivery);
    if (count !== undefined) { pending -= count; deliveries.delete(data.delivery); }
  } else if (data.type === 'error') {
    fail(String(data.message || 'Audio playback consumer failed'));
  } else if ((data.type === 'status' || data.type === 'gate-transition') && data.generation === generation && identity) {
    const channels = identity.channels;
    post({ ...data, type: 'stats', frames: packets, rms, bufferedSamples: Number(data.available) / channels,
      streamKey: `${identity.epoch}:${identity.channel}`, inputIndex: (previousSource.num / previousSource.den).toString(), audioIndex: audioIndex.toString(),
      totalDelayUnknown: Boolean(identity.flags & 0x10000), causalPresentation: Boolean(identity.flags & 0x20000) });
  }
}
function deliver(samples: Float32Array, channels: number): void {
  const frames = samples.length / channels;
  if (frames === 0) return;
  if (pending + frames > targetRate * MAX_QUEUE_SECONDS) {
    fail('Audio consumer is too slow: delivery queue exceeded three seconds');
    return;
  }
  pending += frames;
  queuedAudio.push({ samples, channels, offset: 0 });
  drainAudio();
}
function drainAudio(): void {
  while (queuedAudio.length) {
    const item = queuedAudio[0];
    const remaining = (item.samples.length - item.offset) / item.channels;
    const frames = Math.min(remaining, flowControl ? Math.max(0, sinkLimit - sentFrames) : remaining);
    if (frames === 0) return;
    const count = frames * item.channels;
    // Transfer whole blocks without copying. A credit boundary needs its own
    // backing buffer so transferring the prefix preserves the retained tail.
    const samples = item.offset === 0 && count === item.samples.length
      ? item.samples : item.samples.slice(item.offset, item.offset + count);
    item.offset += count;
    if (item.offset === item.samples.length) queuedAudio.shift();
    const delivery = nextDelivery++;
    deliveries.set(delivery, frames);
    sentFrames += frames;
    output({ type: 'samples', samples, stereo: item.channels === 2, delivery, generation }, [samples.buffer]);
  }
}
function receive(buffer: ArrayBuffer): void {
  if (buffer.byteLength < 96) throw new Error('Truncated AUD2 header');
  const view = new DataView(buffer);
  if (view.getUint32(0, true) !== 0x32445541 || view.getUint32(4, true) !== 1) {
    throw new Error('Expected AUD2 version 1 audio');
  }
  const seq = view.getUint32(8, true);
  const frames = view.getUint32(12, true);
  const next: Identity = {
    rate: view.getFloat64(16, true), tuned: view.getFloat64(24, true),
    channel: view.getUint32(32, true), epoch: view.getUint32(36, true),
    delay: view.getFloat64(48, true), channels: view.getUint32(56, true),
    inputRate: view.getFloat64(72, true), flags: view.getUint32(60, true),
  };
  const inputIndex = view.getBigUint64(40, true);
  const index = view.getBigUint64(64, true);
  const num = view.getBigUint64(80, true);
  const den = view.getBigUint64(88, true);
  if (!Number.isFinite(next.rate) || next.rate <= 0 || !Number.isFinite(next.inputRate) || next.inputRate <= 0
    || !Number.isFinite(next.tuned) || next.tuned < 0 || !Number.isFinite(next.delay) || next.delay < 0
    || next.channel === 0 || (next.channels !== 1 && next.channels !== 2)
    || (next.flags & 0xffff) !== 1 || (next.flags & ~0x30001) !== 0 || den === 0n || num >= den
    || buffer.byteLength !== 96 + frames * next.channels * 4) {
    throw new Error('Invalid AUD2 geometry or source metadata');
  }
  const position = fraction(inputIndex * den + num, den);
  if (seq === 0) {
    const sameIdentity = identity !== null && identity.channel === next.channel && identity.epoch === next.epoch;
    const geometryChanged = identity !== null && (identity.rate !== next.rate || identity.channels !== next.channels);
    const olderIdentity = identityHighWater !== null && (next.epoch < identityHighWater.epoch
      || (next.epoch === identityHighWater.epoch && next.channel <= identityHighWater.channel));
    if (frames !== 0 || (sameIdentity ? !geometryChanged || index !== 0n : olderIdentity)) {
      throw new Error('Invalid AUD2 identity or codec geometry boundary');
    }
    identityHighWater = { epoch: next.epoch, channel: next.channel };
    identity = next;
    sequence = 0;
    audioIndex = index;
    source = position;
    previousSource = position;
    const inputRate = rateFraction(next.inputRate);
    const rate = rateFraction(next.rate);
    sourceStep = fraction(inputRate.num * rate.den, inputRate.den * rate.num);
    flush();
    modeBoundaryPending = false;
    output({ type: 'buffer-mode', stereo: next.channels === 2, mode: playbackMode });
    muted = false;
    post({ type: 'identity', channelId: next.channel, epoch: next.epoch, streamKey: `${next.epoch}:${next.channel}`,
      inputIndex: inputIndex.toString(), audioIndex: index.toString(), delaySeconds: next.delay,
      totalDelayUnknown: Boolean(next.flags & 0x10000), causalPresentation: Boolean(next.flags & 0x20000) });
    return;
  }
  if (!identity || identity.channel !== next.channel || identity.epoch !== next.epoch
    || identity.rate !== next.rate || identity.channels !== next.channels || identity.inputRate !== next.inputRate
    || identity.tuned !== next.tuned) {
    throw new Error('AUD2 identity or geometry changed without a boundary');
  }
  if (frames === 0 || frames > next.rate * MAX_QUEUE_SECONDS || sequence === 0xffffffff || seq !== sequence + 1) {
    throw new Error('AUD2 sequence gap or invalid frame count');
  }
  const causal = Boolean(next.flags & 0x20000);
  const sourceValid = causal || position.num * source.den === source.num * position.den;
  if (index !== audioIndex || !sourceValid) {
    throw new Error('AUD2 source or audio index discontinuity');
  }
  const samples = new Float32Array(frames * next.channels);
  let sum = 0;
  for (let i = 0; i < samples.length; i++) {
    const sample = view.getFloat32(96 + i * 4, true);
    if (!Number.isFinite(sample)) throw new Error('Non-finite AUD2 PCM sample');
    samples[i] = sample;
    sum += sample * sample;
  }
  sequence = seq;
  audioIndex += BigInt(frames);
  previousSource = position;
  source = fraction(position.num * sourceStep.den + BigInt(frames) * sourceStep.num * position.den, position.den * sourceStep.den);
  identity = next;
  rms = Math.sqrt(sum / samples.length);
  packets++;
  if (enabled && !muted && !modeBoundaryPending) {
    deliver(resampler.process(samples, next.channels, next.rate, targetRate), next.channels);
  }
  if (packets % 25 === 0) post({ type: 'stats', frames: packets, rms, totalDelayUnknown: Boolean(next.flags & 0x10000),
    causalPresentation: causal, delaySeconds: next.delay, inputIndex: inputIndex.toString(), audioIndex: index.toString() });
}
function connect(): void {
  if (socket || !enabled || failed) return;
  identity = null;
  identityHighWater = null;
  const current = new WebSocket(url);
  socket = current;
  current.binaryType = 'arraybuffer';
  current.onmessage = (event: MessageEvent) => {
    if (socket !== current) return;
    try {
      if (!(event.data instanceof ArrayBuffer)) throw new Error('Expected binary AUD2 audio');
      receive(event.data);
    } catch (error) { fail(error instanceof Error ? error.message : String(error)); }
  };
  current.onerror = () => { if (socket === current) fail('Backend audio connection failed'); };
  current.onclose = () => { if (socket === current) fail('Backend audio connection closed'); };
}
self.onmessage = (event: MessageEvent) => {
  const message: Record<string, unknown> = event.data && typeof event.data === 'object' ? event.data : {};
  if (message.type === 'init') {
    if (url) { fail('Audio worker initialized more than once'); return; }
    url = String(message.url || '');
    receiverMode = String(message.mode || 'default');
    playbackMode = message.mode === 'drm' || message.mode === 'drmplus' ? 'drm' : 'default';
    targetRate = Number(message.targetRate);
    flowControl = message.flowControl === true;
    if (!Number.isFinite(targetRate) || targetRate <= 0) { fail('Invalid playback sample rate'); return; }
    if (message.port && message.port instanceof MessagePort) {
      port = message.port;
      port.onmessage = (event: MessageEvent) => acceptStatus(event.data || {});
      port.start();
    }
  } else if (message.type === 'mode') {
    const next = String(message.mode || 'default');
    if (next !== receiverMode) {
      receiverMode = next;
      playbackMode = next === 'drm' || next === 'drmplus' ? 'drm' : 'default';
      if (identity) {
        // The UI mode precedes the backend boundary. Its old PCM must stay muted
        // even if the retune watchdog expires while the new codec is acquiring.
        modeBoundaryPending = true;
        flush();
      }
    }
  } else if (message.type === 'enabled') {
    enabled = Boolean(message.value);
    if (!enabled) {
      const previous = socket;
      socket = null;
      previous?.close();
      identity = null;
      modeBoundaryPending = false;
      flush();
    } else if (!socket) { failed = false; connect(); }
    output({ type: 'enabled', value: enabled && !failed });
  } else if (message.type === 'mute') {
    const nextMuted = Boolean(message.value);
    if (nextMuted && !muted) {
      flush();
    }
    muted = nextMuted;
  } else if (message.type === 'reset') {
    flush();
  } else if (message.type === 'detach') {
    port = null;
    flush();
  } else {
    acceptStatus(message);
  }
};
