/**
 * Amplitude chain consumer tests: partial SET_AMP commits, stale-status races, and
 * strict actual-readout rendering. These exercise the real ampState module against the
 * real DOM convention from index.html (static select options); only the ws transport is
 * mocked, and the sent payload is asserted only alongside visible selector state.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));

import { send } from '../core/wsSend';
import { resetAll } from '../core/params';
import { deviceMode } from '../ui/graphMode';
import { AMP_FIELDS, setAmpField, syncAmpAvailability, syncAmpStatus } from '../ui/ampState';

function buildDom(): void {
	for (const field of Object.keys(AMP_FIELDS) as (keyof typeof AMP_FIELDS)[]) {
		const sel = document.createElement('select');
		sel.id = AMP_FIELDS[field].id;
		if (field === 'atten') {
			for (let v = -1; v <= 33; v++) sel.add(new Option(v === -1 ? 'Auto' : v + ' dB', String(v)));
			sel.value = '-1';
		} else if (field === 'preamp') {
			sel.add(new Option('Auto', '0'));
			sel.add(new Option('Off', '1'));
			sel.value = '0';
		} else if (field === 'ifgain') {
			for (const v of ['0', '1', '2', '3']) sel.add(new Option(v, v));
			sel.value = '2';
		} else {
			sel.add(new Option('Low Noise', '0'));
			sel.add(new Option('High Linearity', '1'));
			sel.value = '0';
		}
		document.body.appendChild(sel);
	}
	for (const id of ['cur-atten', 'cur-preamp', 'cur-ifgain']) {
		const span = document.createElement('span');
		span.id = id;
		span.textContent = '-';
		document.body.appendChild(span);
	}
}

const sel = (id: string) => document.getElementById(id) as HTMLSelectElement;
const span = (id: string) => document.getElementById(id) as HTMLElement;

beforeEach(() => {
	buildDom();
	resetAll('amp');
	deviceMode.confirm('std');
	syncAmpStatus({ atten: -1, preamp: 0, ifgain: 2, gain_strategy: 0 });
	vi.mocked(send).mockClear();
});

afterEach(() => {
	for (const id of [...Object.values(AMP_FIELDS).map((m) => m.id), 'cur-atten', 'cur-preamp', 'cur-ifgain']) {
		document.getElementById(id)?.remove();
	}
});

describe('amp field commit', () => {

	it('rejects an out-of-range value and sends nothing', () => {
		const ifgain = sel('select-ifgain');
		const rogue = new Option('9', '9');
		ifgain.add(rogue);
		ifgain.value = '9';
		expect(setAmpField('ifgain')).toBe(false);
		rogue.remove();
		expect(send).not.toHaveBeenCalled();
	});
});

describe('stale status vs pending intent', () => {
	it('a stale report does not revert a fresh selection', () => {
		sel('select-atten').value = '10';
		setAmpField('atten');
		// A pre-intent report arrives late: atten still at the old value.
		syncAmpStatus({ atten: -1, preamp: 0, ifgain: 2, gain_strategy: 0 });
		expect(sel('select-atten').value).toBe('10');
	});

	it('an independent pending selection survives a status that confirms only another field', () => {
		sel('select-ifgain').value = '3';
		setAmpField('ifgain');
		syncAmpStatus({ atten: 0, ifgain: 2 });
		expect(sel('select-ifgain').value).toBe('3');
		expect(sel('select-atten').value).toBe('0'); // no intent for atten, confirmed wins
		expect(send).toHaveBeenLastCalledWith({ cmd: 'SET_AMP', ifgain: 3 });
	});

});

describe('actual readouts render strictly', () => {
	it('null / string / out-of-range actuals never render', () => {
		syncAmpStatus({ atten: -1, atten_actual: null, preamp_actual: '0', ifgain_actual: 99 });
		expect(span('cur-atten').textContent).toBe('');
		expect(span('cur-preamp').textContent).toBe('');
		expect(span('cur-ifgain').textContent).toBe('');
	});

	it('blanks the attenuation sentinel and renders actual zero', () => {
		syncAmpStatus({ atten_actual: -1 });
		expect(span('cur-atten').textContent).toBe('');
		syncAmpStatus({ atten_actual: 0 });
		expect(span('cur-atten').textContent).toBe('0 dB');
	});

	it('numeric actuals render with units', () => {
		syncAmpStatus({ atten_actual: 20, ifgain_actual: 2 });
		expect(span('cur-atten').textContent).toBe('20 dB');
		expect(span('cur-ifgain').textContent).toBe('L2');
	});

	it('preamp actual 0 is Auto (not On); 1 is Off; missing stays blank', () => {
		syncAmpStatus({ preamp_actual: 0 });
		expect(span('cur-preamp').textContent).toBe('Auto');
		syncAmpStatus({ preamp_actual: 1 });
		expect(span('cur-preamp').textContent).toBe('Off');
		syncAmpStatus({});
		expect(span('cur-preamp').textContent).toBe('');
	});

	it.each([5, 25, 33])('keeps legal attenuation %i selected through confirmation', (atten) => {
		syncAmpStatus({ atten });
		expect(sel('select-atten').value).toBe(String(atten));
		sel('select-ifgain').value = '3';
		setAmpField('ifgain');
		syncAmpStatus({ atten, ifgain: 2 });
		expect(sel('select-atten').value).toBe(String(atten));
		expect(sel('select-ifgain').value).toBe('3');
	});
});

describe('selector availability', () => {
	it('manual atten (desired wins stale status) disables preamp only', () => {
		syncAmpStatus({ atten: -1 }); // confirmed Auto, preamp stays enabled
		expect(sel('select-preamp').disabled).toBe(false);
		sel('select-atten').value = '20';
		setAmpField('atten');
		syncAmpStatus({ atten: -1 }); // stale status, desired manual
		expect(sel('select-preamp').disabled).toBe(true);
		expect(sel('select-atten').disabled).toBe(false);
	});

	it('exclusive session disables every amp selector', () => {
		syncAmpAvailability(true);
		for (const m of Object.values(AMP_FIELDS)) expect(sel(m.id).disabled).toBe(true);
		syncAmpAvailability(false);
		expect(sel('select-atten').disabled).toBe(false);
	});

	it('status preserves exclusive measurement ownership', () => {
		deviceMode.confirm('harmonic');
		try {
			syncAmpStatus({ atten: -1 });
			for (const m of Object.values(AMP_FIELDS)) expect(sel(m.id).disabled).toBe(true);
		} finally {
			deviceMode.confirm('std');
		}
	});
});
