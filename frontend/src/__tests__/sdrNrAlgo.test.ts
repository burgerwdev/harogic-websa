/**
 * The NR algorithm selector: state, persistence and the strength-control hiding.
 *
 * DeepFilterNet3 has no strength knob, so the Light/Medium/Strong select must disappear when it is
 * selected and return when Wiener is. The backend confirms the selected algorithm through STATUS;
 * the preference is persisted with the other SDR receiver settings.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderSdrState, resetSdrState, sdrNrAlgo } from '../ui/sdrState';

beforeEach(() => {
	localStorage.clear();
	resetSdrState();
});

/** Mount just the two NR selects `renderSdrState` touches, and remove them after. */
function mountNrControls() {
	const algo = document.createElement('select');
	algo.id = 'select-sdr-nr-algo';
	algo.innerHTML = '<option value="wiener">Wiener</option><option value="dfn">DeepFilterNet3</option>';
	const strength = document.createElement('select');
	strength.id = 'select-sdr-nr-strength';
	strength.innerHTML =
		'<option value="0.25">Light</option><option value="0.6">Medium</option><option value="1">Strong</option>';
	document.body.append(algo, strength);
	return () => {
		algo.remove();
		strength.remove();
	};
}

describe('sdrNrAlgo', () => {
    it('persists the backend-accepted algorithm across a page reload', async () => {
        sdrNrAlgo.set('dfn');
        sdrNrAlgo.confirm('dfn');
        // Re-evaluate module initialization to model a page reload reading persisted state;
        // a static import would reuse the existing in-memory parameter slots.
        vi.resetModules();
        const reloaded = await import('../ui/sdrState');
        expect(reloaded.sdrNrAlgo.get()).toBe('dfn');
        const unmount = mountNrControls();
        reloaded.renderSdrState();
        expect((document.getElementById('select-sdr-nr-algo') as HTMLSelectElement).value).toBe('dfn');
        expect((document.getElementById('select-sdr-nr-strength') as HTMLSelectElement).hidden).toBe(true);
        unmount();
	});


	it('renders the algorithm and hides the strength control only for dfn', () => {
		const unmount = mountNrControls();
		const algo = document.getElementById('select-sdr-nr-algo') as HTMLSelectElement;
		const strength = document.getElementById('select-sdr-nr-strength') as HTMLSelectElement;

		sdrNrAlgo.set('dfn');
		renderSdrState();
		expect(algo.value).toBe('dfn');
		expect(strength.hidden).toBe(true);
		expect(strength.disabled).toBe(true);

		sdrNrAlgo.set('wiener');
		renderSdrState();
		expect(algo.value).toBe('wiener');
		expect(strength.hidden).toBe(false);
		expect(strength.disabled).toBe(false);
		expect(strength.value).toBe('0.6');

		unmount();
	});
});
