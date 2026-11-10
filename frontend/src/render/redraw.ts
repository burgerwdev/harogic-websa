/**
 * Redraw seam.
 *
 * spectrum.ts registers its renderer at module initialization; UI and renderer modules
 * request repaint through this leaf. Do not import application code here.
 */

type Renderer = () => void;

let renderer: Renderer | null = null;
let requests = 0;
let hiddenRequest = false;
let wasHidden = document.hidden;
let streamTimer: ReturnType<typeof setTimeout> | undefined;
let streamFrame: number | undefined;
let lastPaint = -Infinity;
let paintCost = 0;
let paints = 0, streamPaints = 0, paintMs = 0;
// Display work gets a bounded share of the main thread. Ingestion, trace
// accumulation, waterfall history, triggers and media clocks remain independent.
// Canvas commands also incur browser raster/compositing work after returning.
// Allow an ordinary 5 ms paint to keep up with a 30 Hz stream. A 5%
// budget turned small paints into 100–200 ms pauses in every graph mode.
// Only expensive paints back off; there is still at most one pending frame.
const DISPLAY_SHARE = .20;
const MIN_INTERVAL = 1000 / 30;
const MAX_INTERVAL = 100;

function cancelStreamPaint(): void {
  clearTimeout(streamTimer); streamTimer = undefined;
  if (streamFrame !== undefined) cancelAnimationFrame(streamFrame);
  streamFrame = undefined;
}
function paint(stream = false): void {
  const started = performance.now();
  try { renderer?.(); }
  finally {
    const cost = Math.max(0, performance.now() - started);
    paints++; if (stream) streamPaints++; paintMs += cost;
    paintCost = paintCost ? paintCost * .8 + cost * .2 : cost;
    lastPaint = started;
  }
}

document.addEventListener('visibilitychange', () => {
  const hidden = document.hidden;
  if (hidden) cancelStreamPaint();
  if (!hidden && (hiddenRequest || wasHidden)) { hiddenRequest = false; paint(); }
  wasHidden = hidden;
});

/** Called once by the renderer owner (render/spectrum.ts). */
export function setRenderer(fn: Renderer | null): void {
	cancelStreamPaint(); lastPaint = -Infinity; paintCost = 0;
	renderer = fn;
}

// Render synchronously so state changes are painted before requestRender returns.
/** Ask for a repaint. Safe before the renderer is registered (no-op). */
export function requestRender(): void {
	requests++;
	cancelStreamPaint();
	if (document.hidden) { hiddenRequest = true; return; }
	hiddenRequest = false; wasHidden = false;
	paint();
}

/** Data arrivals publish state immediately; the next display reads the newest state. */
export function requestStreamRender(): void {
  requests++;
  if (document.hidden) { hiddenRequest = true; return; }
  if (streamTimer !== undefined || streamFrame !== undefined) return;
  const interval = streamRenderInterval();
  streamTimer = setTimeout(() => {
    streamTimer = undefined;
    streamFrame = requestAnimationFrame(() => {
      streamFrame = undefined;
      if (document.hidden) { hiddenRequest = true; return; }
      hiddenRequest = false; paint(true);
    });
  }, Math.max(0, interval - (performance.now() - lastPaint)));
}

/** Share the display deadline with preparation of replaceable spectrum frames. */
export function streamRenderInterval(): number {
  return Math.min(MAX_INTERVAL, Math.max(MIN_INTERVAL, paintCost / DISPLAY_SHARE));
}

/** Read-only diagnostics; no JSON is built during normal painting. */
export function renderStatus(): Record<string, number | boolean> {
  return { requests, paints, streamPaints, paintMs, meanPaintMs: paintCost,
    intervalMs: streamRenderInterval(),
    pending: streamTimer !== undefined || streamFrame !== undefined };
}

/** Diagnostics for tests/e2e: how many repaints were requested. */
export function renderRequestCount(): number {
	return requests;
}

/** Whether the renderer owner has registered a renderer. */
export function hasRenderer(): boolean {
	return renderer !== null;
}
