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

/** Ask for a repaint. Safe before the renderer is registered (no-op). */
export function requestRender(): void {
	requests++;
	renderer?.();
}

/** Diagnostics for tests/e2e: how many repaints were requested. */
export function renderRequestCount(): number {
	return requests;
}
