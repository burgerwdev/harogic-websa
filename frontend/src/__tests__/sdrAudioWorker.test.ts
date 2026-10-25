import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

interface Message { type: string; [key: string]: unknown }
interface WorkerScope { onmessage: ((event: { data: Message }) => void) | null; postMessage(message: Message): void }
let posted: Message[];
let scope: WorkerScope;
let sockets: FakeSocket[];
class FakeSocket {
  binaryType = '';
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) { sockets.push(this); }
  close(): void {}
}
function packet(options: { seq: number; frames?: number; index?: bigint; input?: bigint; num?: bigint; den?: bigint; epoch?: number; channels?: number; rate?: number; flags?: number } ): ArrayBuffer {
  const frames = options.frames ?? 0;
  const channels = options.channels ?? 1;
  const buffer = new ArrayBuffer(96 + frames * channels * 4);
  const view = new DataView(buffer);
  view.setUint32(0, 0x32445541, true);
  view.setUint32(4, 1, true);
  view.setUint32(8, options.seq, true);
  view.setUint32(12, frames, true);
  view.setFloat64(16, options.rate ?? 48000, true);
  view.setFloat64(24, 100000000, true);
  view.setUint32(32, 7, true);
  view.setUint32(36, options.epoch ?? 1, true);
  view.setBigUint64(40, options.input ?? 0n, true);
  view.setFloat64(48, 0.01, true);
  view.setUint32(56, channels, true);
  view.setUint32(60, options.flags ?? 1, true);
  view.setBigUint64(64, options.index ?? 0n, true);
  view.setFloat64(72, 96000, true);
  view.setBigUint64(80, options.num ?? 0n, true);
  view.setBigUint64(88, options.den ?? 1n, true);
  for (let i = 0; i < frames * channels; i++) view.setFloat32(96 + i * 4, channels === 2 && i % 2 ? -0.25 : 0.5, true);
  return buffer;
}
function send(message: Message): void { scope.onmessage!({ data: message }); }
function receive(buffer: ArrayBuffer): void { sockets[0].onmessage!({ data: buffer }); }
beforeEach(async () => {
  vi.resetModules();
  posted = [];
  sockets = [];
  scope = { onmessage: null, postMessage: (message) => posted.push(message) };
  vi.stubGlobal('self', scope);
  vi.stubGlobal('WebSocket', FakeSocket);
  // Import after stubbing the worker global: module initialization installs its message handler.
  await import('../audio/sdrAudioWorker');
  send({ type: 'init', url: 'ws://localhost/ws/audio?channel=2', targetRate: 48000 });
  send({ type: 'enabled', value: true });
});
afterEach(() => vi.unstubAllGlobals());
describe('AUD2 audio ingress', () => {
  it('preserves stereo and source positions beyond Number integer precision', () => {
    const input = 9007199254740993n;
    receive(packet({ seq: 0, input, num: 1n, den: 3n, channels: 2 }));
    receive(packet({ seq: 1, input, num: 1n, den: 3n, channels: 2, frames: 2 }));
    receive(packet({ seq: 2, input: input + 4n, num: 1n, den: 3n, index: 2n, channels: 2, frames: 1 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    const blocks = posted.filter((message): message is Message & { samples: Float32Array } => message.type === 'samples' && message.samples instanceof Float32Array);
    expect(Array.from(blocks[0].samples)).toEqual([0.5, -0.25, 0.5, -0.25]);
    expect(Array.from(blocks[1].samples)).toEqual([0.5, -0.25]);
  });
  it.each(['sequence', 'source', 'index'])('rejects %s discontinuity or overlapping source samples', (kind) => {
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, frames: 2 }));
    receive(packet({ seq: kind === 'sequence' ? 3 : 2, frames: 2, input: kind === 'source' ? 3n : 4n, index: kind === 'index' ? 3n : 2n }));
    expect(posted.filter((message) => message.type === 'error')).toHaveLength(1);
    expect(posted[posted.length - 1]?.message).toMatch(/gap|discontinuity/);
  });
  it('preserves an NR tail gap with continuous audio indices and exact rational source positions', () => {
    const input = 9007199254740993n;
    receive(packet({ seq: 0, input, num: 1n, den: 3n }));
    receive(packet({ seq: 1, input, num: 1n, den: 3n, frames: 2 }));
    receive(packet({ seq: 2, input: input + 515n, num: 1n, den: 3n, index: 2n, frames: 2 }));
    receive(packet({ seq: 3, input: input + 519n, num: 1n, den: 3n, index: 4n, frames: 2 }));
    expect(posted.filter(message => message.type === 'error')).toEqual([]);
    expect(posted.filter(message => message.type === 'samples')).toHaveLength(3);
    receive(packet({ seq: 4, input: input + 523n, num: 1n, den: 4n, index: 6n, frames: 1 }));
    expect(posted.at(-1)?.message).toMatch(/discontinuity/);
  });
  it('flushes and accepts codec geometry changes only at a new identity boundary', () => {
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, frames: 2 }));
    receive(packet({ seq: 0, epoch: 2, channels: 2, rate: 24000 }));
    receive(packet({ seq: 1, epoch: 2, channels: 2, rate: 24000, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(posted.filter((message) => message.type === 'samples').map((message) => message.stereo)).toEqual([false, true]);
  });
  it.each([
    ['am', 'drmplus'], ['drm', 'drmplus'], ['drmplus', 'am'], ['am', 'nfm'],
  ])('keeps retiring %s PCM muted until the %s stream boundary', (previousMode, nextMode) => {
    send({ type: 'mode', mode: previousMode });
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, frames: 960 }));
    const beforeTransition = posted.filter((message) => message.type === 'samples').length;
    send({ type: 'mode', mode: nextMode });
    send({ type: 'mute', value: true });
    // The UI watchdog may expire before a DRM decoder acquires its new stream.
    send({ type: 'mute', value: false });
    send({ type: 'mode', mode: nextMode });
    const transitionStart = posted.length;
    for (let seq = 2; seq <= 5; seq++) {
      const index = 960n + BigInt(seq - 2) * 48000n;
      receive(packet({ seq, frames: 48000, index, input: index * 2n }));
    }
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(beforeTransition);
    expect(posted.slice(transitionStart).some((message) => message.type === 'buffer-mode')).toBe(false);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    receive(packet({ seq: 0, epoch: 2, rate: 24000, flags: 0x30001 }));
    receive(packet({ seq: 1, epoch: 2, rate: 24000, flags: 0x30001, frames: 2 }));
    const resumed = posted.filter((message) => message.type === 'samples');
    expect(resumed).toHaveLength(beforeTransition + 1);
    expect(Array.from(resumed[resumed.length - 1].samples as Float32Array)).toEqual([0.5, 0.5, 0.5]);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
  });
  it('continues the current stream when its receiver mode is confirmed again', () => {
    send({ type: 'mode', mode: 'am' });
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, frames: 960 }));
    send({ type: 'mode', mode: 'am' });
    receive(packet({ seq: 2, frames: 960, index: 960n, input: 1920n }));
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(2);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
  });
  it('rejects PCM before the stream boundary', () => {
    receive(packet({ seq: 1, frames: 1 }));
    expect(posted[posted.length - 1]?.message).toMatch(/without a boundary/);
  });
  it('fails a consumer that does not acknowledge its bounded delivery queue', () => {
    receive(packet({ seq: 0 }));
    for (let seq = 1; seq <= 4; seq++) receive(packet({ seq, frames: 48000, input: BigInt(seq - 1) * 96000n, index: BigInt(seq - 1) * 48000n }));
    expect(posted[posted.length - 1]?.message).toMatch(/consumer is too slow/);
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(3);
  });
  it('preserves receiver failure and ignores late PCM until an explicit restart', () => {
    receive(packet({ seq: 0 }));
    sockets[0].onclose!({ code: 1011, reason: 'receiver failed' });
    receive(packet({ seq: 1, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([
      { type: 'error', message: 'Backend audio receiver failed' },
    ]);
    expect(posted.filter((message) => message.type === 'samples')).toEqual([]);
    expect(sockets).toHaveLength(1);
    send({ type: 'enabled', value: true });
    expect(sockets).toHaveLength(2);
  });
  it('exposes unknown total delay and causal presentation without claiming exact alignment', () => {
    receive(packet({ seq: 0, flags: 0x30001 }));
    receive(packet({ seq: 1, flags: 0x30001, frames: 2 }));
    const identity = posted.find((message) => message.type === 'identity');
    expect(identity?.totalDelayUnknown).toBe(true);
    expect(identity?.causalPresentation).toBe(true);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
  });
  it('rejects unknown format flag bits', () => {
    receive(packet({ seq: 0, flags: 0x40001 }));
    expect(posted[posted.length - 1]?.message).toMatch(/Invalid AUD2/);
  });
  it('restarts PCM geometry within the same source identity', () => {
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, frames: 2 }));
    receive(packet({ seq: 0, channels: 2, rate: 24000 }));
    receive(packet({ seq: 1, channels: 2, rate: 24000, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(posted.filter((message) => message.type === 'samples').map((message) => message.stereo)).toEqual([false, true]);
  });
  it('rejects a geometry change without a marker', () => {
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, channels: 2, frames: 2 }));
    expect(posted[posted.length - 1]?.message).toMatch(/without a boundary/);
  });
  it('preserves causal provenance anchors independently of PCM interpolation or source ordering', () => {
    receive(packet({ seq: 0, flags: 0x30001, input: 100n }));
    receive(packet({ seq: 1, flags: 0x30001, input: 100n, frames: 2 }));
    receive(packet({ seq: 2, flags: 0x30001, input: 1000n, index: 2n, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    receive(packet({ seq: 3, flags: 0x30001, input: 999n, index: 4n, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(3);
  });
  it('allows timing knowledge flags to change without discarding PCM configuration', () => {
    receive(packet({ seq: 0 }));
    receive(packet({ seq: 1, flags: 0x10001, frames: 2 }));
    receive(packet({ seq: 2, input: 4n, index: 2n, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(2);
  });
  it('accepts a late subscriber marker at a nonzero global audio index', () => {
    const index = 9007199254740995n;
    receive(packet({ seq: 0, index, input: 400n }));
    receive(packet({ seq: 1, index, input: 400n, frames: 2 }));
    receive(packet({ seq: 2, index: index + 2n, input: 404n, frames: 2 }));
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(posted.filter((message) => message.type === 'samples')).toHaveLength(2);
  });
  it('rejects an older source identity after tuning forward', () => {
    receive(packet({ seq: 0, epoch: 3 }));
    receive(packet({ seq: 0, epoch: 4 }));
    receive(packet({ seq: 0, epoch: 2 }));
    expect(posted[posted.length - 1]?.message).toMatch(/Invalid AUD2 identity/);
  });
  it.each<[string, number, number]>([
    ['am', 1, 48000], ['nfm', 1, 24000], ['wfm', 2, 48000],
  ])('keeps %s retune audio continuous through delayed PCM delivery', async (mode, channels, rate) => {
    type Processor = {
      port: { onmessage(event: { data: Message }): void };
      playing: boolean; underruns: number; failed: boolean; playedFrames: number;
      process(inputs: unknown, outputs: Float32Array[][]): void;
    };
    let createProcessor: (() => Processor) | undefined;
    vi.stubGlobal('sampleRate', 48000);
    vi.stubGlobal('AudioWorkletProcessor', class {
      port = { onmessage: null, postMessage: (message: Message) => send(message) };
    });
    vi.stubGlobal('registerProcessor', (_name: string, Constructor: new () => Processor) => {
      createProcessor = () => new Constructor();
    });
    // Registration requires the browser globals above, so static loading cannot work.
    // @ts-expect-error - AudioWorklet modules run with browser-provided globals.
    await import('../audio/sdrAudioWorklet.js');
    const processor = createProcessor!();
    let delivered = 0;
    const drain = () => {
      while (delivered < posted.length) processor.port.onmessage({ data: posted[delivered++] });
    };
    drain();
    send({ type: 'mode', mode });
    receive(packet({ seq: 0, channels, rate }));
    send({ type: 'mute', value: true });
    receive(packet({ seq: 0, epoch: 2, channels, rate }));
    drain();
    const block = rate / 50;
    const outputs = [[new Float32Array(128), new Float32Array(128)]];
    let seq = 0;
    let nextDeliveryAt = 960;
    let started = false;
    for (let t = 0; t < 5 * 48000; t += 128) {
      // A 100 ms delivery delay after three seconds must not interrupt playback.
      if (t < 3 * 48000 || t >= 3.1 * 48000) {
        while (nextDeliveryAt <= t) {
          const index = BigInt(seq * block);
          receive(packet({ seq: ++seq, epoch: 2, channels, rate, frames: block,
            index, input: index * BigInt(96000 / rate) }));
          nextDeliveryAt += 960;
        }
        drain();
      }
      const before = processor.playedFrames;
      processor.process(null, outputs);
      if (started) expect(processor.playedFrames - before).toBe(128);
      if (processor.playing) started = true;
    }
    expect(started).toBe(true);
    expect(processor.underruns).toBe(0);
    expect(processor.failed).toBe(false);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
  });

  it('plays small native codec chunks with DRM priming and still rejects a stalled consumer at the existing ceiling', async () => {
    type Processor = {
      port: { onmessage(event: { data: Message }): void };
      prime: number; maxFill: number; failed: boolean;
      process(inputs: unknown, outputs: Float32Array[][]): void;
    };
    let createProcessor: (() => Processor) | undefined;
    vi.stubGlobal('sampleRate', 48000);
    vi.stubGlobal('AudioWorkletProcessor', class {
      port = { onmessage: null, postMessage: (message: Message) => send(message) };
    });
    vi.stubGlobal('registerProcessor', (_name: string, Constructor: new () => Processor) => {
      createProcessor = () => new Constructor();
    });
    // Static loading cannot register the processor before its browser globals are installed.
    // @ts-expect-error - AudioWorklet modules run with browser-provided globals.
    await import('../audio/sdrAudioWorklet.js');
    const processor = createProcessor!();
    let delivered = 0;
    const drain = () => {
      while (delivered < posted.length) processor.port.onmessage({ data: posted[delivered++] });
    };
    drain();
    receive(packet({ seq: 0 }));
    send({ type: 'mode', mode: 'drm' });
    receive(packet({ seq: 0, epoch: 2, flags: 0x30001 }));
    for (let seq = 1; seq <= 8; seq++) {
      receive(packet({ seq, epoch: 2, frames: 9600, flags: 0x30001, index: BigInt(seq - 1) * 9600n }));
      drain();
    }
    expect(processor.prime).toBe(76800);
    expect(processor.maxFill).toBe(115200);
    const left = new Float32Array(128), right = new Float32Array(128);
    processor.process(null, [[left, right]]);
    expect(left[127]).toBeGreaterThan(0);
    expect(right[127]).toBe(left[127]);
    for (let seq = 9; seq <= 13; seq++) {
      receive(packet({ seq, epoch: 2, frames: 9600, flags: 0x30001, index: BigInt(seq - 1) * 9600n }));
      drain();
    }
    expect(processor.failed).toBe(true);
    expect(posted.find((message) => message.type === 'error')?.message).toMatch(/playback queue limit exceeded/);
  });

  it('preserves every PCM frame across jittered DRM bursts using existing sink credit', async () => {
    type Processor = {
      port: { onmessage(event: { data: Message }): void };
      available: number; received: number; consumedFrames: number; failed: boolean;
      process(inputs: unknown, outputs: Float32Array[][]): void;
    };
    let createProcessor: (() => Processor) | undefined;
    vi.stubGlobal('sampleRate', 48000);
    vi.stubGlobal('AudioWorkletProcessor', class {
      port = { onmessage: null, postMessage: (message: Message) => send(message) };
    });
    vi.stubGlobal('registerProcessor', (_name: string, Constructor: new () => Processor) => {
      createProcessor = () => new Constructor();
    });
    // Processor registration requires browser globals before loading its module.
    // @ts-expect-error - AudioWorklet modules run with browser-provided globals.
    await import('../audio/sdrAudioWorklet.js');
    const processor = createProcessor!();
    let delivered = 0;
    const drain = () => {
      while (delivered < posted.length) processor.port.onmessage({ data: posted[delivered++] });
    };
    // Initialize a fresh ingress instance with the production credit protocol.
    send({ type: 'enabled', value: false });
    vi.resetModules();
    await import('../audio/sdrAudioWorker');
    send({ type: 'init', url: 'ws://localhost/ws/audio', targetRate: 48000, mode: 'drm', flowControl: true });
    send({ type: 'enabled', value: true });
    drain();
    const incoming = (buffer: ArrayBuffer) => sockets[sockets.length - 1].onmessage!({ data: buffer });
    incoming(packet({ seq: 0, flags: 0x30001 }));
    drain();
    const outputs = [[new Float32Array(128), new Float32Array(128)]];
    let seq = 0;
    const times = [0, 1.2, 2.37, 3.6, 4.77, 6, 7.17, 8.4, 9.57, 10.8];
    for (let t = 0; t < 15 * 48000; t += 128) {
      while (seq < times.length && t >= times[seq] * 48000) {
        incoming(packet({ seq: seq + 1, frames: 57600, index: BigInt(seq) * 57600n, flags: 0x30001 }));
        seq++;
      }
      drain();
      processor.process(null, outputs);
      expect(processor.available).toBeLessThanOrEqual(115200);
    }
    drain();
    expect(processor.failed).toBe(false);
    expect(posted.filter((message) => message.type === 'error')).toEqual([]);
    expect(processor.received).toBe(times.length * 57600);
    expect(processor.consumedFrames + processor.available).toBe(times.length * 57600);
    const sent = posted.filter((message) => message.type === 'samples');
    expect(sent.reduce((total, message) => total + (message.samples as Float32Array).length, 0)).toBe(times.length * 57600);
  });
});
