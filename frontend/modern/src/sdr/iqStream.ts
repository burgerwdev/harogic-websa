// SDR IQ stream — the DSP worker's lifecycle, parameters and diagnostics.
//
// The main thread owns neither the socket nor the DSP: it starts/stops the worker, pushes the
// demodulator parameters the backend confirmed, and mirrors what the worker reports. Once the
// worker says its WASM pipeline is producing PCM, the AudioWorklet port is handed to it, so
// playback is fed by the browser DSP; until then the Python audio path keeps playing.
//
// A separate module from the audio path because the IQ ingress is a different stream with a
// different lifetime: it runs while SDR mode is active, independent of whether the speaker is on.
import { routeWorkletPortTo } from '../audio/sdrAudio';
import { wasmDspAllowed, wasmDspReason } from './capability';
import { dspWasmUrl } from './wasm';
import type { PipelineParams } from './wasmPipeline';

let worker: Worker | null = null;
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
let dropped = 0;
let flushes = 0;
let pcmFrames = 0;
let pcmSamples = 0;
let pipelineReady = false;
let handedOff = false;
let lastError = '';
//: Why the browser DSP is off (empty when it is on): 'requested', 'no-webassembly',
//: 'stored-preference' or a fetch/instantiation failure. The Python path is playing in that case.
let fallbackReason = '';
//: The digital path's output: the last decoded message and the timing it came with.
let ft8Text = '';
let ft8Count = 0;
let ft8Detail = '';

/** IQ-only WebSocket URL for the worker (the display connection carries no IQ). */
function iqWorkerUrl(): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const token = sessionStorage.getItem('web-sa-token');
  const q = token ? `&token=${encodeURIComponent(token)}` : '';
  return `${protocol}//${location.host}/ws?iq=1${q}`;
}

/** Publish the worker's counters where the e2e and a bug report can read them. */
function publishIqDebug(): void {
  const cv = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (cv) {
    const ms = rate > 0 ? (samples / rate) * 1000 : 0;
    cv.dataset.sdrIq =
      `enabled=${enabled} blocks=${blocks} samples=${samples} buffered_ms=${ms.toFixed(0)}` +
      ` rate=${rate.toFixed(0)} center_hz=${centerHz.toFixed(0)} dropped=${dropped}` +
      ` flushes=${flushes} pcm_frames=${pcmFrames} pcm_samples=${pcmSamples}` +
      ` pipeline=${pipelineReady}` + (lastError ? ` error=${lastError}` : '') +
      ` fallback=${fallbackReason} ft8=${ft8Count}`;
  }
}

function startWorker(): void {
  if (worker) return;
  // jsdom (unit tests) and pre-Worker browsers have no DSP host. That is not an error: the
  // Python-side DSP path is the documented fallback when the browser cannot run the pipeline.
  if (typeof Worker === 'undefined') return;
  try {
    worker = new Worker(new URL('./iqWorker.ts', import.meta.url), { type: 'module' });
  } catch {
    worker = null;
    return;
  }
  worker.onmessage = (event: MessageEvent) => {
    const d = (event.data || {}) as Record<string, any>;
    if (d.type === 'error') {
      lastError = String(d.message || 'error');
      publishIqDebug();
      return;
    }
    if (d.type === 'ft8') {
      renderFt8Message(d as unknown as Ft8Report);
      return;
    }
    if (d.type !== 'stats') return;
    if (typeof d.blocks === 'number') blocks = d.blocks;
    if (typeof d.samples === 'number') samples = d.samples;
    if (typeof d.rate === 'number') rate = d.rate;
    if (typeof d.centerHz === 'number') centerHz = d.centerHz;
    if (typeof d.dropped === 'number') dropped = d.dropped;
    if (typeof d.flushes === 'number') flushes = d.flushes;
    if (typeof d.pcmFrames === 'number') pcmFrames = d.pcmFrames;
    if (typeof d.pcmSamples === 'number') pcmSamples = d.pcmSamples;
    pipelineReady = Boolean(d.pipeline);
    // One handoff, and only once the DSP is actually producing PCM: handing the worklet to a
    // worker whose pipeline failed would silence the radio, which is the failure this ordering
    // (and the Python fallback behind it) exists to prevent.
    if (pipelineReady && !handedOff && worker) {
      handedOff = routeWorkletPortTo(worker);
    }
    publishIqDebug();
  };
  // The browser DSP is used only when the policy allows it: with `?wasm=0` (or without WebAssembly)
  // the worker is started without a module, so it never produces PCM and never takes the worklet
  // port — the Python audio path (`?audio=1`) keeps playing, which is the documented fallback.
  fallbackReason = wasmDspAllowed() ? '' : wasmDspReason();
  worker.postMessage({
    type: 'init',
    url: iqWorkerUrl(),
    wasmUrl: fallbackReason ? '' : dspWasmUrl(),
  });
  worker.postMessage({ type: 'enabled', value: enabled });
}

/** Push the confirmed demodulator parameters (and listener preferences) to the DSP worker. */
export function configureSdrPipeline(
  params: PipelineParams,
  options: { volume?: number; audioEnabled?: boolean } = {},
): void {
  worker?.postMessage({
    type: 'configure',
    params,
    volume: options.volume,
    audioEnabled: options.audioEnabled,
  });
}

/** Volume is a listener preference: applied in the worker, where the PCM is produced. */
export function setSdrPipelineVolume(volume: number): void {
  worker?.postMessage({ type: 'volume', value: volume });
}

/** Turn the WASM audio-enhancement chain on/off (RAW/digital comparisons flip this). */
export function setSdrPipelineAudio(enabled: boolean): void {
  worker?.postMessage({ type: 'audio', value: enabled });
}

/**
 * Enter/leave the IQ ingress. Called on the SDR mode edge: the stream is pointless outside SDR
 * (the backend only produces IQ while an SDR session runs) and a lingering socket would keep the
 * backend encoding raw IQ for nobody.
 */
export function setSdrIqEnabled(on: boolean): void {
  enabled = on;
  if (on) {
    startWorker();
    worker?.postMessage({ type: 'enabled', value: true });
  } else {
    worker?.postMessage({ type: 'enabled', value: false });
  }
  publishIqDebug();
}

/** Drop the worker's stream state (preset/reset): counters restart with the new capture geometry. */
export function resetSdrIq(): void {
  ft8Text = '';
  ft8Count = 0;
  ft8Detail = '';
  const readout = document.getElementById('ft8-readout');
  if (readout) readout.textContent = '—';
  blocks = 0;
  samples = 0;
  dropped = 0;
  flushes = 0;
  rate = 0;
  centerHz = 0;
  pcmFrames = 0;
  pcmSamples = 0;
  lastError = '';
  worker?.postMessage({ type: 'reset' });
  publishIqDebug();
}

/** A decoded FT8 transmission as the worker reports it. */
export interface Ft8Report {
  text: string;
  frequencyHz: number;
  timeOffsetS: number;
  snrDb: number;
  count: number;
}

/**
 * Show a decoded transmission: the text plus the timing it was found at.
 *
 * Both parts are shown because an FT8 message without its slot timing is of little use to an
 * operator watching a band. Exported so the rendering is unit tested without a worker.
 */
export function renderFt8Message(report: Ft8Report): void {
  ft8Text = String(report.text || '');
  ft8Count = Number(report.count) || ft8Count + 1;
  ft8Detail =
    `${ft8Text}  @ ${(Number(report.frequencyHz) || 0).toFixed(0)} Hz, ` +
    `+${(Number(report.timeOffsetS) || 0).toFixed(2)} s, ${(Number(report.snrDb) || 0).toFixed(0)} dB`;
  const readout = document.getElementById('ft8-readout');
  if (readout) readout.textContent = ft8Detail;
  publishIqDebug();
}

/** The last decoded FT8 message (tests and diagnostics). */
export function lastFt8Message(): { text: string; count: number; detail: string } {
  return { text: ft8Text, count: ft8Count, detail: ft8Detail };
}

/** Diagnostics for the e2e and for tests: the transport counters and the DSP state. */
export function sdrIqStats(): {
  enabled: boolean;
  blocks: number;
  samples: number;
  dropped: number;
  pcmFrames: number;
  pipelineReady: boolean;
  fallbackReason: string;
} {
  return { enabled, blocks, samples, dropped, pcmFrames, pipelineReady, fallbackReason };
}
