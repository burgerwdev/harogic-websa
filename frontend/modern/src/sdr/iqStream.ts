// SDR IQ stream — the DSP worker's lifecycle and diagnostics.
//
// The main thread owns neither the socket nor the DSP: it only starts/stops the worker and
// mirrors what the worker reports. A separate module (rather than a branch inside the audio
// path) because the IQ ingress is a different stream with a different lifetime: it runs while
// SDR mode is active, independent of whether the speaker is unmuted.
//
// Task scope: transport + diagnostics. The DSP stages consume the decoded blocks in the worker.
let worker: Worker | null = null;
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
let dropped = 0;
let flushes = 0;

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
      ` flushes=${flushes}`;
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
    if (d.type !== 'stats') return;
    if (typeof d.blocks === 'number') blocks = d.blocks;
    if (typeof d.samples === 'number') samples = d.samples;
    if (typeof d.rate === 'number') rate = d.rate;
    if (typeof d.centerHz === 'number') centerHz = d.centerHz;
    if (typeof d.dropped === 'number') dropped = d.dropped;
    if (typeof d.flushes === 'number') flushes = d.flushes;
    publishIqDebug();
  };
  worker.postMessage({ type: 'init', url: iqWorkerUrl() });
  worker.postMessage({ type: 'enabled', value: enabled });
}

/**
 * Enter/leave the IQ ingress. Called on the SDR mode edge: the stream is pointless outside SDR
 * (the backend only produces IQ while an SDR session runs) and a lingering socket would keep
 * the backend encoding raw IQ for nobody.
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

/** Drop the worker (preset/reset): counters restart with the new capture geometry. */
export function resetSdrIq(): void {
  blocks = 0;
  samples = 0;
  dropped = 0;
  flushes = 0;
  rate = 0;
  centerHz = 0;
  worker?.postMessage({ type: 'reset' });
  publishIqDebug();
}

// Diagnostics for the e2e and for tests that need the transport counters.
export function sdrIqStats(): { enabled: boolean; blocks: number; samples: number; dropped: number } {
  return { enabled, blocks, samples, dropped };
}
