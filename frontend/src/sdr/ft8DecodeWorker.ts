// The FT8 decode worker — the WASM half of the decoder.
//
// This worker owns the demodulator pipeline and nothing else: no socket, so a decode that runs
// for seconds cannot stall the stream (see `ft8Worker.ts` for why that split exists). Baseband
// blocks arrive over a MessagePort (transferred buffers, sequence numbers intact); the seq-gap
// bookkeeping, the windowing comments and the counters below moved here unchanged from the
// worker that used to be both halves.
import { dsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';
import { loadPluginManifest } from './registry';

let module: DspModule | null = null;
let pipeline: WasmPipeline | null = null;
let params: PipelineParams | null = null;
let enabled = false;
let port: MessagePort | null = null;
let lastSeq = -1;
let dropped = 0;
let pushes = 0;
/// Windows the decoder searched (one per slot by design), and how often its buffer was thrown away.
///
/// Both were invisible before, and the two look identical from the buffer length alone: a search
/// drops it by one hop, a reset drops it to zero. Counting a reset as a search is what made a live
/// session unreadable -- 41 "attempts" that could be 41 searches or 20 searches and 21 resets, which
/// is the difference between "the decoder missed 36 transmissions" and "it never had a clean window".
let attempts = 0;
let resets = 0;
/// A reset zeroes the buffer, so the next frame's length looks like a search's drop. Latched here so
/// the reset is not also counted as a search.
let afterReset = false;
/// Decoder buffered length at the previous frame, used to spot that slide.
let lastBuffered = 0;
/// Backlog guard: frames queued while a decode ran. The decode duty cycle is well under real time
/// so the queue drains every slot, but a pathological band (four subtraction passes, say) must not
/// turn memory into the failure mode — drop the oldest half and let the seq bookkeeping reset the
/// window (a torn window decodes nothing anyway; that is the honest outcome).
let queued: { seq: number; iq: Float32Array; centerHz: number }[] = [];
const QUEUED_MAX_MS = 30_000;
let queuedSamples = 0;

/** Drop the decoder's window: a flush or a gap tore the stream, so the slot in it is unrecoverable. */
function forgetWindow(): void {
  pipeline?.reset();
  resets += 1;
  afterReset = true;
}

function post(msg: Record<string, unknown>): void {
  port?.postMessage(msg);
}

self.onmessage = (event: MessageEvent) => {
  const msg = (event.data || {}) as Record<string, any>;
  if (msg.type === 'init') {
    const wasmUrl = String(msg.wasmUrl || '');
    port = (msg.port as MessagePort) ?? null;
    if (port) port.onmessage = (fromStream: MessageEvent) => receive(fromStream.data);
    if (!wasmUrl) return;
    // The registry's loader: it caches the manifest the mode helpers read.
    void loadPluginManifest(wasmUrl)
      .then(() => {
        module = dsp();
        post({ type: 'ready' });
      })
      .catch(() => post({ type: 'error', message: 'dsp.wasm unavailable (FT8)' }));
  } else if (msg.type === 'configure') {
    params = (msg.params as PipelineParams) ?? null;
    enabled = Boolean(msg.enabled);
    build();
  } else if (msg.type === 'stop') {
    pipeline?.free();
    pipeline = null;
  }
};

function onBaseband(frame: { seq: number; iq: Float32Array; centerHz: number }): void {
  if (frame.seq === 0) {
    lastSeq = -1;
    forgetWindow();
    if (frame.iq.length === 0) return;
  } else if (lastSeq >= 0 && frame.seq > lastSeq + 1) {
    dropped += frame.seq - lastSeq - 1;
    forgetWindow();                       // a gap tears a slot: start the next one clean
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
  if (buffered < lastBuffered) {
    if (afterReset) afterReset = false;
    else attempts += 1;                   // the window slid: the decoder searched it
  }
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
      resets,
      attempts,
    });
  }
}

/** Queue a block from the stream worker; drain what is ready before it (arrival order by seq). */
function receive(msg: Record<string, any>): void {
  if (!msg || msg.type !== 'baseband') return;
  const frame = { seq: Number(msg.seq) || 0, iq: msg.iq as Float32Array,
                  centerHz: Number(msg.centerHz) || 0 };
  queued.push(frame);
  queuedSamples += frame.iq.length / 2;
  const rate = params?.fsIn ?? 0;      // the configured baseband rate, for the backlog bound
  if (rate > 0) {
    while (queuedSamples > (QUEUED_MAX_MS / 1000) * rate && queued.length > 2) {
      const gone = queued.shift();
      if (gone) queuedSamples -= gone.iq.length / 2;
    }
  }
  // Drain in order: one `push` per block, exactly the cadence the pipeline saw when the halves
  // were one worker (a decode triggers inside the push that fills the window).
  while (queued.length > 0) {
    const next = queued.shift();
    if (!next) break;
    queuedSamples -= next.iq.length / 2;
    onBaseband(next);
  }
}

/** The geometry the decoder was last built for: a baseband-rate change needs a rebuild (the
 * resampler inside the WASM pipeline is sized by `fsIn → outRate` at creation). */
let shape: { fsIn: number; outRate: number } | null = null;

/** (Re)build the decoder for the current parameters. */
function build(): void {
  if (!module || !enabled || !params) {
    pipeline?.free();
    pipeline = null;
    shape = null;
    return;
  }
  const next = { fsIn: params.fsIn, outRate: params.outRate };
  // Idempotent only while the geometry is really unchanged: a baseband-rate change while FT8 stays
  // selected used to keep the old resampler, feeding the decoder mis-scaled samples (no decode until
  // a hard refresh or a change that forced a rebuild).
  if (pipeline && shape && pipeline.ok && pipeline.mode === params.mode
      && shape.outRate === next.outRate
      && Math.abs(shape.fsIn - next.fsIn) <= Math.max(200, next.fsIn * 0.01)) {
    return;
  }
  pipeline?.free();
  pipeline = null;
  const built = new WasmPipeline(module, params, true);
  if (!built.ok) {
    built.free();
    shape = null;
    post({ type: 'error', message: `no FT8 decoder for mode ${params.mode}` });
    return;
  }
  pipeline = built;
  shape = next;
}
