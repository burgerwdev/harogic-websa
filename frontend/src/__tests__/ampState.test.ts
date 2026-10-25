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
import { AMP_FIELDS, rejectAmpRequest, resetAmpIntents, setAmpField, syncAmpAvailability, syncAmpStatus } from '../ui/ampState';

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
function lastRequestId(): string {
	const command = vi.mocked(send).mock.calls.at(-1)![0];
	if (!('request_id' in command) || typeof command.request_id !== 'string') throw new Error('Missing request ID');
	return command.request_id;
}

function commit(field: keyof typeof AMP_FIELDS, value: number): string {
	sel(AMP_FIELDS[field].id).value = String(value);
	setAmpField(field);
	return lastRequestId();
}

beforeEach(() => {
	vi.useFakeTimers();
	buildDom();
	resetAmpIntents();
	resetAll('amp');
	deviceMode.confirm('std');
	syncAmpStatus({ atten: -1, preamp: 0, ifgain: 2, gain_strategy: 0 });
	vi.mocked(send).mockClear();
});

afterEach(() => {
	resetAmpIntents();
	vi.useRealTimers();
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
		expect(send).toHaveBeenLastCalledWith({ cmd: 'SET_AMP', ifgain: 3, request_id: expect.any(String) });
	});

	it('settles each field independently and ignores unrelated snapshot values', () => {
		const atten = commit('atten', 10);
		const ifgain = commit('ifgain', 3);
		syncAmpStatus({ atten: 10, ifgain: 2 }, 'SET_AMP', atten);
		expect(sel('select-atten').value).toBe('10');
		expect(AMP_FIELDS.atten.slot.pending()).toBe(false);
		expect(sel('select-ifgain').value).toBe('3');
		expect(AMP_FIELDS.ifgain.slot.pending()).toBe(true);
		syncAmpStatus({ atten: -1, ifgain: 3 }, 'SET_AMP', ifgain);
		expect(sel('select-atten').value).toBe('10');
		expect(AMP_FIELDS.ifgain.slot.pending()).toBe(false);
	});

	it('keeps a repeated-value cycle pending until its own ACK and ignores older ACKs afterward', () => {
		const first = commit('ifgain', 3);
		const second = commit('ifgain', 1);
		const newest = commit('ifgain', 3);
		expect(new Set([first, second, newest]).size).toBe(3);
		syncAmpStatus({ ifgain: 3 }, 'SET_AMP', first);
		syncAmpStatus({ ifgain: 3 });
		rejectAmpRequest('SET_AMP', second);
		expect(AMP_FIELDS.ifgain.slot.pending()).toBe(true);
		expect(sel('select-ifgain').value).toBe('3');
		syncAmpStatus({ ifgain: 3 }, 'SET_AMP', newest);
		syncAmpStatus({ ifgain: 1 }, 'SET_AMP', second);
		expect(AMP_FIELDS.ifgain.slot.pending()).toBe(false);
		expect(sel('select-ifgain').value).toBe('3');
	});

	it('rolls back to the newest accepted predecessor on matching ERROR even while focused', () => {
		const first = commit('atten', 5);
		const second = commit('atten', 10);
		const newest = commit('atten', 20);
		syncAmpStatus({ atten: 10 }, 'SET_AMP', second);
		syncAmpStatus({ atten: 5 }, 'SET_AMP', first);
		sel('select-atten').focus();
		rejectAmpRequest('SET_REF', newest);
		rejectAmpRequest('SET_AMP', second);
		expect(sel('select-atten').value).toBe('20');
		rejectAmpRequest('SET_AMP', newest);
		expect(sel('select-atten').value).toBe('10');
		expect(AMP_FIELDS.atten.slot.pending()).toBe(false);
		syncAmpStatus({ atten: 20 }, 'SET_AMP', newest);
		expect(sel('select-atten').value).toBe('10');
	});

	it('repaints focused controls and availability when a request expires without another status', () => {
		const request = commit('atten', 20);
		sel('select-atten').focus();
		expect(sel('select-preamp').disabled).toBe(true);
		vi.advanceTimersByTime(3000);
		expect(sel('select-atten').value).toBe('-1');
		expect(sel('select-preamp').disabled).toBe(false);
		expect(AMP_FIELDS.atten.slot.pending()).toBe(false);
		syncAmpStatus({ atten: 20 }, 'SET_AMP', request);
		expect(sel('select-atten').value).toBe('-1');
	});

	it('restarts only the resubmitted field watchdog for an identical value', () => {
		commit('atten', 20);
		commit('ifgain', 3);
		vi.advanceTimersByTime(2000);
		commit('atten', 20);
		vi.advanceTimersByTime(1000);
		expect(sel('select-atten').value).toBe('20');
		expect(sel('select-ifgain').value).toBe('2');
		vi.advanceTimersByTime(2000);
		expect(sel('select-atten').value).toBe('-1');
		expect(sel('select-preamp').disabled).toBe(false);
	});

	it('cancels all intents on disconnect and ignores their late replies across reconnect', () => {
		const atten = commit('atten', 20);
		const ifgain = commit('ifgain', 3);
		resetAmpIntents();
		expect(sel('select-atten').value).toBe('-1');
		expect(sel('select-ifgain').value).toBe('2');
		expect(sel('select-preamp').disabled).toBe(false);
		syncAmpStatus({ atten: 5, ifgain: 1 });
		syncAmpStatus({ atten: 20, ifgain: 3 }, 'SET_AMP', atten);
		syncAmpStatus({ atten: 20, ifgain: 3 }, 'SET_AMP', ifgain);
		vi.advanceTimersByTime(3000);
		expect(sel('select-atten').value).toBe('5');
		expect(sel('select-ifgain').value).toBe('1');
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

	it.each([2, 3, 4])('shows SDK preamp grade %i without replacing requested policy', (grade) => {
		syncAmpStatus({ preamp: 0, preamp_actual: grade });
		expect(span('cur-preamp').textContent).toBe('L' + grade);
		expect(sel('select-preamp').value).toBe('0');
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
