/**
 * The NR algorithm selector: state, persistence and the strength-control hiding.
 *
 * DeepFilterNet3 has no strength knob, so the Light/Medium/Strong select must disappear when it is
 * selected and return when Wiener is. The algorithm is a client-owned preference (authoritative,
 * persisted with the other SDR preferences) because the backend's Python path is only the fallback.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { createParam } from '../core/params';
import { SDR_PREF_KEYS, renderSdrState, resetSdrState, sdrNrAlgo } from '../ui/sdrState';

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
	it('defaults to wiener and lives in the persisted SDR preferences', () => {
		expect(sdrNrAlgo.get()).toBe('wiener');
		expect(SDR_PREF_KEYS).toContain('web-sa-sdr-nr-algo');
	});

	it('persists the selected algorithm and reads it back', () => {
		sdrNrAlgo.set('dfn');
		expect(localStorage.getItem('web-sa-sdr-nr-algo')).toBe('dfn');
		expect(sdrNrAlgo.get()).toBe('dfn');
	});

	it('coerces a bogus value back to wiener, but maps the old persisted dfn2 key to dfn', () => {
		localStorage.setItem('test-nr-algo', 'not-an-algo');
		const p = createParam<'wiener' | 'dfn'>('test.nrAlgo', {
			fallback: 'wiener',
			scope: 'test',
			persistKey: 'test-nr-algo',
			persist: 'desired',
			authoritative: true,
			parse: (raw: string) => (raw === 'dfn' || raw === 'dfn2' ? 'dfn' : 'wiener'),
			serialize: String,
			equals: (a, b) => a === b,
		});
		expect(p.get()).toBe('wiener');
		localStorage.setItem('test-nr-algo', 'dfn2');
		const p2 = createParam<'wiener' | 'dfn'>('test.nrAlgo2', {
			fallback: 'wiener',
			scope: 'test',
			persistKey: 'test-nr-algo',
			persist: 'desired',
			authoritative: true,
			parse: (raw: string) => (raw === 'dfn' || raw === 'dfn2' ? 'dfn' : 'wiener'),
			serialize: String,
			equals: (a, b) => a === b,
		});
		expect(p2.get()).toBe('dfn');
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
