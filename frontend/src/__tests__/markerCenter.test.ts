/**
 * Marker → Center must speak the language of the mode it is in.
 *
 * Measured on the bench (fake device, Chromium): in SDR mode `S.rtaMode` is true for BOTH
 * rta and sdr, so the press sent `SET_RTA` — a command the backend refuses outside RTA
 * mode. The marker stayed put and the only trace was the command error. Each confirmed
 * graph mode gets its own command here: SET_SDR (with the capture decimate), SET_RTA, or
 * the swept SET_FREQ.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { resetAll } from '../core/params';
import { send } from '../core/wsSend';
import * as S from '../core/store';
import { markerToCenter } from '../ui/panels/markers';
import { graphMode } from '../ui/graphMode';
import { sdrCenterHz, sdrDecimate } from '../ui/sdrState';
import { centerHz, rtaCenterHz, spanHz } from '../ui/freqState';

vi.mock('../core/wsSend', () => ({ send: vi.fn() }));

const FREQS = new Float64Array([100e6, 101e6, 102e6]);

afterEach(() => {
	vi.mocked(send).mockClear();
	localStorage.clear();
	resetAll();
	S.setFreqArray(null);
	graphMode.reset();
});

function armMarker() {
	S.setFreqArray(FREQS);
	const m = S.markers[0];
	m.enabled = true;
	m.idx = 1;
	S.setActiveMkrId(m.id);
}

describe('markerToCenter', () => {
	it('sends SET_SDR in SDR mode and moves the SDR centre', () => {
		armMarker();
		graphMode.confirm('sdr');
		sdrDecimate.set(16);

		markerToCenter();

		const payload = vi.mocked(send).mock.lastCall?.[0] as Record<string, unknown>;
		expect(payload).toMatchObject({ cmd: 'SET_SDR', center: 101e6, decimate: 16 });
		expect(sdrCenterHz.get()).toBe(101e6);
	});

	it('sends SET_RTA in RTA mode and moves the RTA centre', () => {
		armMarker();
		graphMode.confirm('rta');

		markerToCenter();

		const payload = vi.mocked(send).mock.lastCall?.[0] as Record<string, unknown>;
		expect(payload).toMatchObject({ cmd: 'SET_RTA', center: 101e6 });
		expect(rtaCenterHz.get()).toBe(101e6);
	});

	it('sends SET_FREQ in the swept mode and keeps the span', () => {
		armMarker();
		graphMode.confirm('std');
		spanHz.confirm(10e6);

		markerToCenter();

		const payload = vi.mocked(send).mock.lastCall?.[0] as Record<string, unknown>;
		expect(payload).toMatchObject({ cmd: 'SET_FREQ', center: 101e6, span: 10e6 });
		expect(centerHz.get()).toBe(101e6);
	});
});
