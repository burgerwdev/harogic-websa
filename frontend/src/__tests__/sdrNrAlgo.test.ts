/**
 * The NR algorithm selector: state, persistence and the strength-control hiding.
 *
 * DeepFilterNet3 has no strength knob, so the Light/Medium/Strong select must disappear when it is
 * selected and return when Wiener is. The backend confirms the selected algorithm through STATUS;
 * the preference is persisted with the other SDR receiver settings.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { parseNrAlgo, renderSdrState, resetSdrState, sdrNrAlgo, type NrAlgo } from '../ui/sdrState';

beforeEach(() => {
	localStorage.clear();
	resetSdrState();
});

/** Mount just the two NR selects `renderSdrState` touches, and remove them after. */
function mountNrControls() {
	const algo = document.createElement('select');
	algo.id = 'select-sdr-nr-algo';
	algo.innerHTML = ['wiener','dfn','gtcrn','ul-unas','fastenhancer'].map(id => `<option value="${id}">${id}</option>`).join('');
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
    it.each<NrAlgo>(['dfn','gtcrn','ul-unas','fastenhancer'])('persists %s across a page reload', async (id) => {
        sdrNrAlgo.set(id);
        sdrNrAlgo.confirm(id);
        // Re-evaluate module initialization to model a page reload reading persisted state;
        // a static import would reuse the existing in-memory parameter slots.
        vi.resetModules();
        const reloaded = await import('../ui/sdrState');
        expect(reloaded.sdrNrAlgo.get()).toBe(id);
        const unmount = mountNrControls();
        reloaded.renderSdrState();
        expect((document.getElementById('select-sdr-nr-algo') as HTMLSelectElement).value).toBe(id);
        expect((document.getElementById('select-sdr-nr-strength') as HTMLSelectElement).hidden).toBe(true);
        unmount();
	});


	it.each<NrAlgo>(['dfn','gtcrn','ul-unas','fastenhancer'])('renders %s with attenuation instead of Wiener strength', (id) => {
		const unmount = mountNrControls();
		const algo = document.getElementById('select-sdr-nr-algo') as HTMLSelectElement;
		const strength = document.getElementById('select-sdr-nr-strength') as HTMLSelectElement;

		sdrNrAlgo.set(id);
		renderSdrState();
		expect(algo.value).toBe(id);
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
    it('migrates the old DFN id and rejects unknown stored ids', () => {
        expect(parseNrAlgo('dfn2')).toBe('dfn');
        expect(parseNrAlgo('unavailable-model')).toBe('wiener');
    });
});
