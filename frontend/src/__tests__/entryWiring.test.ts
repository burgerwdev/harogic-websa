/**
 * Entry wiring: the registration side effects must actually be reachable.
 *
 * Breaking the render/spectrum cycles meant nothing imports the hub any more, so its
 * `setRenderer(renderAll)` / `registerViewRenderer('rta')` side effects only run if the
 * entry point imports the module. Without that the app renders nothing at all - and it
 * fails silently (no exception, no console error), which is exactly what happened.
 */
import { describe, expect, it } from 'vitest';
import { hasRenderer } from '../render/redraw';
import { registeredViews } from '../render/registry';

// Importing the hub is what wires the renderer (same side effect main.ts relies on).
import '../render/spectrum';

describe('entry wiring', () => {
	it('registers the swept/RTA renderer when the hub module is loaded', () => {
		expect(hasRenderer()).toBe(true);
		expect(registeredViews()).toContain('rta');
	});
});
