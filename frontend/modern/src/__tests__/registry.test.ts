/**
 * View renderer registry (report finding E-5).
 *
 * `renderAll()` used to name every measurement module in an if/elif chain. Views register
 * themselves now, so this test pins both the registry mechanics and the fact that the
 * shipped views actually register when their modules are imported.
 */
import { describe, expect, it } from 'vitest';
import {
	getViewRenderer, registeredViews, registerViewRenderer, unregisterViewRenderer,
} from '../render/registry';

// Importing these modules is what registers their views (side effect on purpose).
import '../meas/phaseNoise';
import '../meas/harmonic';
import '../render/spectrum';

describe('view renderer registry', () => {
	it('registers the shipped measurement views', () => {
		const modes = registeredViews();
		expect(modes).toContain('rta');
		expect(modes).toContain('pnm');
		expect(modes).toContain('harm');
	});

	it('returns the renderer for a mode and undefined otherwise', () => {
		expect(getViewRenderer('pnm')?.mode).toBe('pnm');
		expect(getViewRenderer('does-not-exist')).toBeUndefined();
	});

	it('lets a view register and unregister itself', () => {
		const calls: string[] = [];
		registerViewRenderer({ mode: 'test-view', render: () => calls.push('rendered') });
		getViewRenderer('test-view')?.render({} as never);
		expect(calls).toEqual(['rendered']);
		unregisterViewRenderer('test-view');
		expect(getViewRenderer('test-view')).toBeUndefined();
	});

	it('replaces a renderer when the same mode registers twice', () => {
		registerViewRenderer({ mode: 'test-dupe', render: () => {} });
		registerViewRenderer({ mode: 'test-dupe', render: () => {} });
		expect(registeredViews().filter((m) => m === 'test-dupe')).toHaveLength(1);
		unregisterViewRenderer('test-dupe');
	});
});
