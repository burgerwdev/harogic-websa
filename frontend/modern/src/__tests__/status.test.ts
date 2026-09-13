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
import { resetAll } from '../core/params';
import { centerHz, spanHz, rtaCenterHz, swpCenterHz } from '../ui/freqState';
import { refLevel, refMode } from '../ui/refState';
import { currentPoints, currentRBW, currentSpur, currentVBW, rbwMode, vbwMode } from '../ui/swpState';
import { currentGraphMode } from '../ui/graphMode';
import { sdrCenterHz, sdrDecimate, sdrDemod, sdrIfbw, sdrListenHz } from '../ui/sdrState';

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
	auto_ref: { last_peak: null, last_noise_floor: null, candidate: null, pending: null },
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

describe('updateStatus', () => {
	it('confirms the swept parameters into their slots', () => {
		updateStatus(structuredClone(SWP_STATUS));
		expect(centerHz.get()).toBe(225000000);
		expect(spanHz.get()).toBe(3125000);
		expect(refLevel.get()).toBe(-30);
		expect(refMode.get()).toBe('manual');
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
