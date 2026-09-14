/**
 * STATUS → parameter-slot wiring.
 *
 * `updateStatus()` is the only place where a backend report enters the frontend, and every
 * recurring "the panel disagrees with the device" bug in this project lived here (the
 * parameter-slot model in core/params.ts was introduced for exactly that). The pure slots
 * are unit tested in params.test.ts; this test covers the mapping from the wire payload
 * onto them, which had no test at all (report finding P0-2 step 2).
 *
 * The payload below is a real STATUS captured from a SAN-90, trimmed to the fields the
 * slots consume (kept structurally complete so a shape change fails here).
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { updateStatus } from '../core/ws';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { centerHz, spanHz, rtaCenterHz, swpCenterHz } from '../ui/freqState';
import { refLevel } from '../ui/refState';
import { currentPoints, currentRBW, currentSpur, currentVBW, rbwMode, vbwMode } from '../ui/swpState';
import { currentGraphMode } from '../ui/graphMode';
import { sdrAudioOn, sdrCenterHz, sdrDecimate, sdrDemod, sdrIfbw, sdrListenHz } from '../ui/sdrState';
import { noticeText } from '../core/store';

const SWP_STATUS = {
	cmd: 'STATUS',
	connected: true,
	center: 225000000,
	span: 3125000,
	ref: -30,
	ref_mode: 'manual',
	rbw: 100000,
	rbw_mode: 'manual',
	vbw: 100000,
	vbw_mode: 'manual',
	points: 103,
	window: 1,
	spur: 'bypass',
	detector: 'auto',
	sweep_time_mode: 0,
	sweep_time: 0,
	mode: 'std',
	config_version: 189,
	caps: { model: 67, name: 'SAN-90', fmin: 8000, fmax: 9020000000 },
	req: {
		center: 225000000, span: 3125000, points: 1000,
		rbw_mode: 'manual', rbw: 100000, vbw_mode: 'manual', vbw: 100000,
		ref_mode: 'manual', ref: -30, sweep_time_mode: 0, sweep_time: 0,
		spur: 'bypass', detector: 'auto', window: 1, rta_center: 100200000,
		swp: {
			center: 225000000, span: 3125000, points: 1000,
			rbw_mode: 'manual', rbw: 100000, vbw_mode: 'manual', vbw: 100000,
			ref_mode: 'manual', ref: -30, sweep_time_mode: 0, sweep_time: 0,
			spur: 'bypass', detector: 'auto', window: 1,
		},
		rta: {
			center: 100200000, span: 12695312.5, points: 3328,
			rbw_mode: 'auto', rbw: 0, vbw_mode: 'equal', vbw: 0,
			ref_mode: 'manual', ref: 0, sweep_time_mode: 2, sweep_time: 0,
			trigger_source: 'bus', trigger_edge: 'rising', trigger_level: -40,
			trigger_safetime: 0, trigger_delay: 0, trigger_pretime: 0, trigger_acqtime: 0.005,
			trigger_retrigger: 0, trigger_retriggerperiod: 0, trigger_out: 'none',
			trigger_outpolarity: 'positive', trigger_actual: {},
		},
		sdr: {
			center: 100200000, decimate: 16, listen: 101762500, demod: 'am',
			if_bw: 12000, squelch: -110, volume: 0.8, agc: true, pitch: 700, deemph_us: -1,
		},
	},
	actual: { center: 225000000, span: 3125000, start: 223437500, stop: 226562500, ref: -30, rbw: 100000, vbw: 100000, points: 103, est_min: 0 },
	sdr: { health: {} },
	auto_ref: { last_peak: null, last_noise_floor: null, target: null, result: 'idle',
		pending: null, adjusting: false },
	rta_health: { error_streak: 0, recovery_attempts: 0 },
};

const SDR_STATUS = {
	...SWP_STATUS,
	mode: 'sdr',
	center: 100200000,
	span: 3125000,
	points: 1639,
	req: {
		...SWP_STATUS.req,
		center: 100200000,
		span: 3125000,
		points: 1639,
		rbw: 1907.3,
	},
	actual: { ...SWP_STATUS.actual, center: 100200000, bandwidth: 3125000, pan_points: 1639 },
	sdr: { ...SWP_STATUS.req.sdr, actual: { bandwidth: 3125000, pan_points: 1639 }, level_dbfs: -60, health: {} },
};

beforeEach(() => {
	resetAll();
	localStorage.clear();
});

describe('a reference level the device did not accept', () => {
	// (what the UI asked the profile for, what the hardware programmed and echoed back). The pair
	// is only test data: at runtime the value comes from `actual.ref` in the STATUS, which is why
	// several pairs are checked - a hard-coded level would pass a single case.
	const limited = [[30, 27], [30, 25], [25, 22], [-5, -8]] as const;

	it.each(limited)('names the level the device actually programmed (%i -> %i)', (asked, reported) => {
		const status = structuredClone(SWP_STATUS) as any;
		status.req.swp.ref = asked;
		status.actual.ref = reported;
		status.ref = reported;
		updateStatus(status);
		expect(noticeText).toBe(`Device limited Ref to ${reported} dBm`);
	});

	it('follows the device when the limit moves', () => {
		const post = (asked: number, reported: number) => {
			const status = structuredClone(SWP_STATUS) as any;
			status.req.swp.ref = asked;
			status.actual.ref = reported;
			status.ref = reported;
			updateStatus(status);
		};
		post(30, 26);
		expect(noticeText).toBe('Device limited Ref to 26 dBm');
		post(30, 24);                                  // the device picked another attenuation
		expect(noticeText).toBe('Device limited Ref to 24 dBm');
	});

	it('is not repeated while the same limit stands', () => {
		const status = structuredClone(SWP_STATUS) as any;
		status.req.swp.ref = 30;
		status.actual.ref = 23;
		status.ref = 23;
		updateStatus(status);
		expect(noticeText).toBe('Device limited Ref to 23 dBm');
		S.setNoticeText('something else');            // the user's own message is not overwritten
		updateStatus(status);
		expect(noticeText).toBe('something else');
	});

	it('says nothing when the device accepted the request', () => {
		const status = structuredClone(SWP_STATUS) as any;
		status.req.swp.ref = -20;
		status.actual.ref = -20;
		status.ref = -20;
		S.setNoticeText('');                          // nothing posted means this is left alone
		updateStatus(status);
		expect(noticeText).toBe('');
	});
});

describe("the user's SDR audio preference", () => {
	it('survives leaving and re-entering SDR', () => {
		// Reported: with audio on, a visit to RTA/SWP turned it off for good (the mode-exit path
		// wrote the preference off). Leaving a mode stops the runtime, not the user's setting.
		const sdr = (mode: string) => {
			const status = structuredClone(SWP_STATUS) as any;
			status.mode = mode;
			updateStatus(status);
		};
		sdr('sdr');
		sdrAudioOn.set(true);
		sdr('rta');
		expect(sdrAudioOn.get()).toBe(true);
		sdr('std');
		expect(sdrAudioOn.get()).toBe(true);
		sdr('sdr');
		expect(sdrAudioOn.get()).toBe(true);
		// ...and an explicit "off" stays off, so the check above cannot pass by accident.
		sdrAudioOn.set(false);
		sdr('rta');
		sdr('sdr');
		expect(sdrAudioOn.get()).toBe(false);
	});
});

describe('updateStatus', () => {
	it('confirms the swept parameters into their slots', () => {
		updateStatus(structuredClone(SWP_STATUS));
		expect(centerHz.get()).toBe(225000000);
		expect(spanHz.get()).toBe(3125000);
		expect(refLevel.get()).toBe(-30);
		expect(rbwMode.get()).toBe('manual');
		expect(currentRBW.get()).toBe(100000);
		expect(currentVBW.get()).toBe(100000);
		expect(vbwMode.get()).toBe('manual');
		expect(currentPoints.get()).toBe(103);    // the DEVICE-native count (req.points is 1000)
		expect(currentSpur.get()).toBe('bypass');
		// confirmed, not merely requested: no intent is left pending
		expect(centerHz.pending()).toBe(false);
		expect(currentPoints.pending()).toBe(false);
	});

	it('keeps the RTA window separate from the swept window', () => {
		updateStatus(structuredClone(SWP_STATUS));
		expect(rtaCenterHz.get()).toBe(100200000);
		expect(swpCenterHz.get()).toBe(225000000);   // hand-off source for SDR
		expect(currentGraphMode()).toBe('std');
	});

	it('follows the mode reported by the backend', () => {
		updateStatus(structuredClone(SDR_STATUS));
		expect(currentGraphMode()).toBe('sdr');
		expect(sdrCenterHz.get()).toBe(100200000);
		expect(sdrDecimate.get()).toBe(16);
		expect(sdrListenHz.get()).toBe(101762500);
		expect(sdrDemod.get()).toBe('am');
		expect(sdrIfbw.get()).toBe(12000);
	});

	it('does not confirm a parameter that was not reported', () => {
		const partial = structuredClone(SWP_STATUS) as Record<string, unknown>;
		updateStatus(partial);
		const before = spanHz.get();
		delete partial.req;
		delete (partial as { actual?: unknown }).actual;
		updateStatus(partial);                        // malformed: the guard must reject it
		expect(spanHz.get()).toBe(before);
	});
});


describe('IF-overflow warning repaint', () => {
	it('repaints when the warning appears and when it clears', async () => {
		const { renderRequestCount } = await import('../render/redraw');
		const overflow = structuredClone(SWP_STATUS) as Record<string, any>;
		overflow.status_warning = -12;

		const before = renderRequestCount();
		updateStatus(overflow);
		const afterAppear = renderRequestCount();
		// The overflowing IF sends no frames, so the canvas would otherwise keep the previous
		// pass and never show the warning (the user saw it only after raising Ref again).
		expect(afterAppear).toBeGreaterThan(before);

		updateStatus(structuredClone(SWP_STATUS));      // status_warning back to 0
		const afterClear = renderRequestCount();
		expect(afterClear).toBeGreaterThan(afterAppear);

		// a repeat of the same state must not repaint on every STATUS
		updateStatus(structuredClone(SWP_STATUS));
		expect(renderRequestCount()).toBe(afterClear);
	});
});

describe('device-disconnect warning', () => {
	// The reported bug: after an unplug the trace froze while STATUS still said connected. The
	// link now flips to false; the canvas must say so (and repaint on its own, since a
	// disconnected device sends no frame to drive the frame loop).
	it('marks the device disconnected and repaints on the transition', async () => {
		const { renderRequestCount } = await import('../render/redraw');
		const gone = structuredClone(SWP_STATUS) as Record<string, any>;
		gone.connected = false;

		const before = renderRequestCount();
		updateStatus(gone);
		expect(S.deviceConnected).toBe(false);
		expect(S.statusWarnings.join(' ')).toContain('DEVICE DISCONNECTED');
		const afterAppear = renderRequestCount();
		expect(afterAppear).toBeGreaterThan(before);

		updateStatus(structuredClone(SWP_STATUS));      // reconnected
		expect(S.deviceConnected).toBe(true);
		expect(S.statusWarnings.join(' ')).not.toContain('DEVICE DISCONNECTED');
		const afterClear = renderRequestCount();
		expect(afterClear).toBeGreaterThan(afterAppear);

		// a repeat of the same (connected) state must not repaint on every STATUS
		updateStatus(structuredClone(SWP_STATUS));
		expect(renderRequestCount()).toBe(afterClear);
	});
});

describe('waterfall is disabled while measuring', () => {
	it('turns the waterfall off, disables the buttons, and restores the choice afterwards', async () => {
		const { applyMeasUI } = await import('../ui/measureUi');
		const { waterfallOn } = await import('../ui/waterfallState');
		const { setWaterfall } = await import('../ui/panels/waterfall');
		const button = document.createElement('button');
		button.id = 'btn-waterfall';
		const pause = document.createElement('button');
		pause.id = 'btn-wf-pause';
		document.body.append(button, pause);

		setWaterfall(true);
		expect(waterfallOn.get()).toBe(true);

		S.setMeasOn(true);
		applyMeasUI();
		expect(waterfallOn.get()).toBe(false);
		expect(button.disabled).toBe(true);
		expect(pause.disabled).toBe(true);

		S.setMeasOn(false);
		applyMeasUI();
		expect(waterfallOn.get()).toBe(true);        // the user's choice comes back
		expect(button.disabled).toBe(false);
		button.remove();
		pause.remove();
	});
});
