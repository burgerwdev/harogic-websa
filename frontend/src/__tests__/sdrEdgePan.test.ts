/**
 * SDR edge push: the move of the capture centre that a drag past the plot sides starts.
 *
 * Reported: a drag near either side of the spectrum raised "Device: center must be a number". The
 * message came once for each push, 350 ms apart, and every dialog had to be dismissed. The push
 * put the parameter SLOTS on the wire (`center: sdrCenterHz`). JSON made objects of them, and the
 * number validator of the backend refused the whole command. These tests pin the two properties
 * that correct this: the payload carries numbers, and an unknown span cannot make a bad centre.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { resetAll } from '../core/params';
import { send } from '../core/wsSend';
import { sdrPanCentreBy } from '../ui/controls';
import { sdrCenterHz, sdrDecimate, sdrSpanHz } from '../ui/sdrState';

vi.mock('../core/wsSend', () => ({ send: vi.fn() }));

afterEach(() => {
	vi.mocked(send).mockClear();
	localStorage.clear();
	resetAll();
});

/** The values STATUS would have confirmed before the drag: centre + capture span. */
function armed() {
	sdrCenterHz.set(100e6);
	sdrSpanHz.confirm(3.125e6);
	sdrDecimate.set(16);
}

describe('SDR edge push', () => {
	it('sends a numeric centre and decimate', () => {
		armed();
		sdrPanCentreBy(0.2);
		const payload = vi.mocked(send).mock.lastCall?.[0] as Record<string, unknown>;
		expect(payload).toMatchObject({ cmd: 'SET_SDR' });
		expect(typeof payload.center).toBe('number');
		expect(typeof payload.decimate).toBe('number');
		expect(payload.center).toBe(100e6 + 3.125e6 * 0.2);
		expect(payload.decimate).toBe(16);
	});

	it('pans down by the same fraction with a numeric centre', () => {
		armed();
		sdrPanCentreBy(-0.2);
		const payload = vi.mocked(send).mock.lastCall?.[0] as Record<string, unknown>;
		expect(payload.center).toBe(100e6 - 3.125e6 * 0.2);
		// The slot follows the request, so the next push moves from the new centre.
		expect(sdrCenterHz.get()).toBe(100e6 - 3.125e6 * 0.2);
	});

	it('sends nothing while the capture span is unknown', () => {
		// Before the first STATUS the span is 0. There is no distance to move, and a centre computed
		// from it must not reach the device. The function uses `confirm` and not `reset` because the
		// span is confirm-only: the fallback applies only while nothing has been reported.
		sdrSpanHz.confirm(0);
		sdrCenterHz.set(100e6);
		sdrDecimate.set(16);
		sdrPanCentreBy(0.2);
		expect(send).not.toHaveBeenCalled();
	});
});
