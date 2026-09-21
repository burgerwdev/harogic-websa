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
import { dsp, type DspModule } from './wasm';
import { WasmPipeline, type PipelineParams } from './wasmPipeline';
import { loadPluginManifest } from './registry';

let module: DspModule | null = null;
let pipeline: WasmPipeline | null = null;
let params: PipelineParams | null = null;
let enabled = false;
let pushes = 0;

function post(msg: Record<string, unknown>): void {
  (self as unknown as { postMessage: (m: unknown) => void }).postMessage(msg);
}

self.onmessage = (event: MessageEvent) => {
  const msg = (event.data || {}) as Record<string, any>;
  if (msg.type === 'init') {
    const wasmUrl = String(msg.wasmUrl || '');
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
  } else if (msg.type === 'frame') {
    if (!enabled || !pipeline || !(msg.iq instanceof Float32Array)) return;
    pushes += 1;
    const decoded = pipeline.push(msg.iq as Float32Array);
    post({
      type: 'ft8-stats',
      pushes: pushes,
      buffered: pipeline.buffered(),
      decodes: pipeline.count(),
    });
    if (decoded) post({ type: 'ft8', ...decoded, centerHz: msg.centerHz, count: decoded.count });
  } else if (msg.type === 'stop') {
    pipeline?.free();
    pipeline = null;
  }
};

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
