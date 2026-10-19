/**
 * Redraw seam.
 *
 * `render/spectrum.ts` is the orchestrator: it imports every renderer (markers, peak list,
 * measurement overlays, waterfall). Those renderers, and the UI modules that change the
 * state they draw, used to import `renderAll` back out of it - fourteen module cycles, with
 * `spectrum.ts` as the hub (docs/.../ARCH_REVIEW.md finding P1-4).
 *
 * The dependency is inverted here: `spectrum.ts` registers its renderer at module init and
 * everyone else only asks for a repaint. This module is a leaf - it must never import
 * application code, or the cycle comes straight back.
 */

type Renderer = () => void;

let renderer: Renderer | null = null;
let requests = 0;

/** Called once by the renderer owner (render/spectrum.ts). */
export function setRenderer(fn: Renderer | null): void {
	renderer = fn;
}

// Measured on the live bench (2026-10-18, SAN-90, SDR mode): ~18 requests/s steady state,
// rAF gap p95 = 16.8 ms, no frame above 50 ms (`window.websaRenderRequests()` + a rAF
// timeline sampler in the console reproduce it). A rAF coalesce here was considered and
// REJECTED on those numbers: it would add one frame of latency to every marker/trigger
// interaction and break the synchronous render assertions, for no measured gain. Revisit
// only with a profile showing request bursts above the frame budget.
/** Ask for a repaint. Safe before the renderer is registered (no-op). */
export function requestRender(): void {
	requests++;
	renderer?.();
}

/** Diagnostics for tests/e2e: how many repaints were requested. */
export function renderRequestCount(): number {
	return requests;
}

/**
 * True when a renderer is registered. The UI regression asserts this (and that pixels were
 * actually painted): the hub registers itself only if `render/spectrum.ts` is imported, and
 * a missing import once left the canvas blank with no error at all.
 */
export function hasRenderer(): boolean {
	return renderer !== null;
}
