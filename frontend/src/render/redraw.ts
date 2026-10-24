/**
 * Redraw seam.
 *
 * spectrum.ts registers its renderer at module initialization; UI and renderer modules
 * request repaint through this leaf. Do not import application code here.
 */

type Renderer = () => void;

let renderer: Renderer | null = null;
let requests = 0;

/** Called once by the renderer owner (render/spectrum.ts). */
export function setRenderer(fn: Renderer | null): void {
	renderer = fn;
}

// Render synchronously so state changes are painted before requestRender returns.
/** Ask for a repaint. Safe before the renderer is registered (no-op). */
export function requestRender(): void {
	requests++;
	renderer?.();
}

/** Diagnostics for tests/e2e: how many repaints were requested. */
export function renderRequestCount(): number {
	return requests;
}

/** Whether the renderer owner has registered a renderer. */
export function hasRenderer(): boolean {
	return renderer !== null;
}
