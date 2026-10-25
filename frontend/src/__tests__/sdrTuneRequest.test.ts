import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn() }));

import * as S from '../core/store';
import { send } from '../core/wsSend';
import { renderRequestCount } from '../render/redraw';
import { sdrListenHz } from '../ui/sdrState';
import {
	cancelSdrTuneRequest, confirmSdrTuneReport, rejectSdrTuneRequest,
	requestSdrTune, resetSdrTuneSession, stopSdrTuneRepeat,
} from '../ui/sdrTuneRequest';

type Sent = { freq: number; request_id: string };
// Every assertion reads the command log through the same projection.
const sent = () => vi.mocked(send).mock.calls.map(([command]) => command as Sent);
// Acknowledge one sent command as the reply to exactly that request.
const own = (command: Sent) => confirmSdrTuneReport(command.freq, 'SET_SDR_CHANNEL', command.request_id, undefined);

beforeEach(() => {
	vi.useFakeTimers();
	localStorage.clear();
	sdrListenHz.confirm(0);   // test world: the backend has not reported a listen frequency
	vi.mocked(send).mockClear();
});

afterEach(() => {
	resetSdrTuneSession();    // also disarms any still-running watchdog timer
	vi.clearAllTimers();
	vi.useRealTimers();
	localStorage.clear();
});

describe('SDR listen-frequency tuning requests', () => {
	it('sends once for repeats while a request is pending, then exactly the last target', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.105e6);
		requestSdrTune(7.110e6);
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.get()).toBe(7.110e6);

		own(sent()[0]);
		expect(sent().map(({ freq }) => freq)).toEqual([7.100e6, 7.110e6]);
		own(sent()[1]);
		expect(sent()).toHaveLength(2);
		// Fully drained: no pending intent is left that a later reply could act on.
		expect(sdrListenHz.pending()).toBe(false);
	});

	it('keeps only the newest target through a fast reversal', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.200e6);
		requestSdrTune(7.050e6);
		own(sent()[0]);
		expect(sent().map(({ freq }) => freq)).toEqual([7.100e6, 7.050e6]);
		own(sent()[1]);
		expect(sdrListenHz.get()).toBe(7.050e6);
		expect(sdrListenHz.confirmedValue()).toBe(7.050e6);
	});

	it('does not drain the queue while only periodic STATUS of the same frequency arrives', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		for (let i = 0; i < 3; i++) {
			vi.advanceTimersByTime(800);
			expect(confirmSdrTuneReport(7.100e6, 'STATUS', undefined, undefined)).toBe(true);
			expect(sdrListenHz.get()).toBe(7.110e6);
		}
		expect(S.statusError).toBeNull();          // no watchdog fired along the way
		expect(sent()).toHaveLength(1);
		// A periodic STATUS echoing our request id is still not the reply of record.
		expect(confirmSdrTuneReport(7.100e6, 'STATUS', sent()[0].request_id, 0)).toBe(true);
		expect(sent()).toHaveLength(1);
		own(sent()[0]);
		expect(sent().map(({ freq }) => freq)).toEqual([7.100e6, 7.110e6]);
	});

	it('ignores replies that do not match the in-flight request identity', () => {
		requestSdrTune(7.100e6);
		const [a] = sent();
		// An ack for a request this session never had must not even confirm the slot.
		expect(confirmSdrTuneReport(7.000e6, 'SET_SDR_CHANNEL', 'sdr-tune-0', undefined)).toBe(false);
		expect(sdrListenHz.confirmedValue()).toBe(0);
		// A foreign namespace's numeric id confirms the report but cannot clear pending state.
		expect(confirmSdrTuneReport(7.000e6, 'SET_SDR_CHANNEL', 42, undefined)).toBe(true);
		expect(sdrListenHz.get()).toBe(7.100e6);
		// Nor can a reply to a different command.
		expect(confirmSdrTuneReport(7.000e6, 'SET_SDR', a.request_id, undefined)).toBe(true);
		expect(sdrListenHz.get()).toBe(7.100e6);
		expect(own(a)).toBe(true);
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.pending()).toBe(false);
	});

	it('never resurrects the old queue after a session or mode cancel', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		resetSdrTuneSession();
		own(sent()[0]);          // a late ack must not start anything new
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.get()).toBe(0);
		// Plain cancel (without the session version reset) is the same one-way door.
		requestSdrTune(7.200e6);
		requestSdrTune(7.300e6);
		cancelSdrTuneRequest();
		own(sent()[1]);
		expect(sent().map(({ freq }) => freq)).toEqual([7.100e6, 7.200e6]);
		expect(sdrListenHz.get()).toBe(0);
	});

	it('discards the queue and restores the confirmed value on rejection', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		rejectSdrTuneRequest('SET_SDR_CHANNEL', 'sdr-tune-0');   // wrong id: no effect
		rejectSdrTuneRequest('SET_SDR', sent()[0].request_id);   // wrong command: no effect
		expect(sdrListenHz.get()).toBe(7.110e6);
		rejectSdrTuneRequest('SET_SDR_CHANNEL', sent()[0].request_id);
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.get()).toBe(0);
		expect(sdrListenHz.pending()).toBe(false);
		own(sent()[0]);          // a late ack after the rejection must not start anything new
		expect(sent()).toHaveLength(1);
		// Fresh input after the rejection works immediately again.
		requestSdrTune(7.140e6);
		own(sent()[1]);
		expect(sdrListenHz.get()).toBe(7.140e6);
	});

	it('times out after 3000 ms without an ack, discarding the queue', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		vi.advanceTimersByTime(2999);
		expect(sdrListenHz.get()).toBe(7.110e6);
		expect(S.statusError).toBeNull();
		vi.advanceTimersByTime(1);
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.get()).toBe(0);
		expect(S.statusError).toBe('Receiver tuning timed out; restored the confirmed frequency');
		const repaints = renderRequestCount();
		own(sent()[0]);          // the timed-out request is dead; no repaint, no resend
		expect(sent()).toHaveLength(1);
		expect(renderRequestCount()).toBe(repaints);
	});

	it('discards unsent repeats on blur while the current request settles', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		stopSdrTuneRepeat();
		expect(sdrListenHz.get()).toBe(7.100e6);
		own(sent()[0]);
		expect(sent()).toHaveLength(1);
		expect(sdrListenHz.get()).toBe(7.100e6);
		expect(sdrListenHz.confirmedValue()).toBe(7.100e6);
		expect(sdrListenHz.pending()).toBe(false);
	});

	it('ignores reports carrying an older configuration version', () => {
		requestSdrTune(7.100e6);
		requestSdrTune(7.110e6);
		expect(confirmSdrTuneReport(7.100e6, 'STATUS', undefined, 5)).toBe(true);
		expect(confirmSdrTuneReport(7.100e6, 'STATUS', undefined, 3)).toBe(false);
		expect(sdrListenHz.get()).toBe(7.110e6);
		own(sent()[0]);
		expect(sent().map(({ freq }) => freq)).toEqual([7.100e6, 7.110e6]);
		own(sent()[1]);
		expect(sdrListenHz.get()).toBe(7.110e6);
	});

	it('does not resend a value the backend already confirmed', () => {
		requestSdrTune(0);
		requestSdrTune(Number.NaN);
		expect(sent()).toHaveLength(0);
		requestSdrTune(7.100e6);
		own(sent()[0]);
		const count = sent().length;
		requestSdrTune(7.100e6);
		expect(sent()).toHaveLength(count);
		// A sub-half-Hz difference is the same target, not a new command.
		requestSdrTune(7.100e6 + 0.3);
		expect(sent()).toHaveLength(count);
		expect(sdrListenHz.pending()).toBe(false);
	});
});
