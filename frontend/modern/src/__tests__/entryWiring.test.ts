/**
 * Entry wiring: the registration side effects must actually be reachable.
 *
 * Breaking the render/spectrum cycles meant nothing imports the hub any more, so its
 * `setRenderer(renderAll)` / `registerViewRenderer('rta')` side effects only run if the
 * entry point imports the module. Without that the app renders nothing at all - and it
 * fails silently (no exception, no console error), which is exactly what happened.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { hasRenderer } from '../render/redraw';
import { registeredViews } from '../render/registry';

// Importing the hub is what wires the renderer (same side effect main.ts relies on).
import '../render/spectrum';

const ROOT = resolve(process.cwd(), 'src');

describe('entry wiring', () => {
	it('registers the swept/RTA renderer when the hub module is loaded', () => {
		expect(hasRenderer()).toBe(true);
		expect(registeredViews()).toContain('rta');
	});

	it('the entry point imports the hub for its side effects', () => {
		const main = readFileSync(resolve(ROOT, 'main.ts'), 'utf8');
		expect(main).toMatch(/import\s+'\.\/render\/spectrum';/);
	});

	it('the entry point imports the audio worker wiring and the control panels it binds', () => {
		const main = readFileSync(resolve(ROOT, 'main.ts'), 'utf8');
		// guards the same failure mode for the other side-effectful imports
		expect(main).toMatch(/import\s+\{\s*initStore\s*\}/);
		expect(main).toMatch(/import\s+\{\s*connectWS\s*\}/);
	});
});
