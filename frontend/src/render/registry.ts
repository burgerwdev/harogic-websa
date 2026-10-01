/**
 * View renderer registry (report finding E-5).
 *
 * `renderAll()` used to dispatch on `viewMode` with an if/elif chain that named every
 * measurement module, so adding a view meant editing the renderer hub. Views register
 * themselves here instead and the hub only looks up the active mode; the swept path is the
 * default when nothing is registered.
 *
 * This module is a leaf (no application imports) so a view module can register without
 * creating a cycle back into render/spectrum.ts.
 */

/** Drawing primitives the hub lends to a view that needs the swept canvas. */
export interface ViewContext {
	renderGrid: () => void;
	renderTraceLine: (trace: any) => void;
	getDisplayPowers: () => Float32Array | null;
}

export interface ViewRenderer {
	/** View mode this renderer draws (matches store.viewMode). */
	mode: string;
	/** Draw one full pass of the view. */
	render: (ctx: ViewContext) => void;
}

const renderers = new Map<string, ViewRenderer>();

export function registerViewRenderer(renderer: ViewRenderer): void {
	renderers.set(renderer.mode, renderer);
}

export function unregisterViewRenderer(mode: string): void {
	renderers.delete(mode);
}

export function getViewRenderer(mode: string): ViewRenderer | undefined {
	return renderers.get(mode);
}

/** Registered modes, in registration order (diagnostics/tests). */
export function registeredViews(): string[] {
	return [...renderers.keys()];
}
