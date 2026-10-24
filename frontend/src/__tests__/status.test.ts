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
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { updateStatus } from '../core/ws';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { centerHz, spanHz, rtaCenterHz, swpCenterHz } from '../ui/freqState';
import { refLevel } from '../ui/refState';
import { currentPoints, currentRBW, currentSpur, currentVBW, rbwMode, vbwMode } from '../ui/swpState';
import { currentGraphMode } from '../ui/graphMode';
import { sdrAudioOn, sdrCenterHz, sdrDecimate, sdrDemod, sdrIfbw, sdrListenHz, sdrSquelch } from '../ui/sdrState';
import { noticeText } from '../core/store';
import { fillGnssDetail } from '../ui/panels/gnss';
import { t } from '../core/i18n';
import { sdrIqStats } from '../sdr/iqStream';
import { setWaterfall } from '../ui/panels/waterfall';

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

describe('GNSS freshness during wideband capture', () => {
	it('marks paused polling and hides stale lock/satellite details', () => {
		document.body.innerHTML = '<span id="info-gnss"></span><span id="gnss-d-lock"></span><span id="gnss-d-sats"></span>';
		const status = structuredClone(SWP_STATUS) as Record<string, any>;
		status.gnss = { lock: 1, sats: 8, stale: true };
		updateStatus(status);
		fillGnssDetail();
		expect(document.getElementById('info-gnss')?.textContent).toBe(t('status_stale'));
		expect(document.getElementById('gnss-d-lock')?.textContent).toBe(t('status_stale'));
		expect(document.getElementById('gnss-d-sats')?.textContent).toBe('-');
	});
});

describe('reference clock hardware capabilities', () => {
	it('removes unavailable premium sources despite live GNSS and preserves requested versus actual', () => {
		document.body.innerHTML = '<select id="select-refclk"><option value="internal">Int</option><option value="external">Ext</option><option value="external_forced">ExtForce</option><option value="premium">Int+ (DOCXO)</option></select><span id="refclk-d-current"></span><span id="refclk-d-actual"></span><div id="refclk-popover"></div>';
		updateStatus({
			...SWP_STATUS, has_docxo: false, has_premium_ref_clock: false,
			gnss: { lock: 1, sats: 22, docxo_mode: 1 }, ref_clock: 'external',
			actual: { ...SWP_STATUS.actual, refclk_src: 0 },
		});
		const select = document.getElementById('select-refclk') as HTMLSelectElement;
		expect(select.querySelector('option[value="premium"]')).toBeNull();
		expect(select.value).toBe('external');
		expect(document.getElementById('refclk-d-current')?.textContent).toBe(t('ext'));
		expect(document.getElementById('refclk-d-actual')?.textContent).toBe(`${t('int')} (${t('refclk_fell_back')})`);
	});

	it('labels OCXO and DOCXO by installed oscillator independently of GNSS lock', () => {
		document.body.innerHTML = '<select id="select-refclk"><option value="internal">Int</option></select><span id="refclk-d-current"></span><span id="refclk-d-actual"></span><div id="refclk-popover"></div>';
		for (const hasDocxo of [false, true]) {
			updateStatus({
				...SWP_STATUS, has_docxo: hasDocxo, has_premium_ref_clock: true,
				gnss: { lock: 0, docxo: 0, docxo_mode: 0 }, ref_clock: 'premium',
				actual: { ...SWP_STATUS.actual, refclk_src: 2 },
			});
			const label = t(hasDocxo ? 'docxo_premium' : 'ocxo_premium');
			expect(document.querySelector('#select-refclk option[value="premium"]')?.textContent).toBe(label);
			expect(document.getElementById('refclk-d-current')?.textContent).toBe(label);
			expect(document.getElementById('refclk-d-actual')?.textContent).toBe(label);
		}
	});

	it('hides DOCXO status and work mode when the oscillator is absent', () => {
		document.body.innerHTML = '<div class="param-row"><span id="gnss-d-docxo"></span></div><div class="param-row"><span id="gnss-d-docxo_mode"></span></div><span id="gnss-d-sats"></span>';
		for (const installed of [false, true, false]) {
			updateStatus({ ...SWP_STATUS, has_docxo: installed, gnss: { lock: 1, sats: 22, docxo_mode: 1 } });
			fillGnssDetail();
			for (const id of ['gnss-d-docxo', 'gnss-d-docxo_mode']) {
				expect(document.getElementById(id)?.closest<HTMLElement>('.param-row')?.hidden).toBe(!installed);
			}
			expect(document.getElementById('gnss-d-sats')?.textContent).toBe('22');
		}
	});
});

describe('independent sweep-time fields', () => {
	it('STATUS refreshes SWT from the device estimate, never the measured EMA', () => {
		const status = structuredClone(SWP_STATUS) as Record<string, any>;
		status.sweep_ms = 12.5;
		status.sweep_ms_meas = 250;
		updateStatus(status);
		expect(S.sweepMs).toBe(12.5);
		status.sweep_ms_meas = 300;
		updateStatus(status);
		expect(S.sweepMs).toBe(12.5);
		expect(S.sweepMsMeas).toBe(300);
		delete status.sweep_ms;  // older backends reported the time only in the frame header
		updateStatus(status);
		expect(S.sweepMs).toBe(12.5);
	});
	it('falls back to publisher duration when SDK estimate is unavailable', async () => {
		const { updateInfoBar } = await import('../render/infobar');
		document.body.innerHTML = '<span id="info-swt"></span>';
		const status = structuredClone(SWP_STATUS) as Record<string, any>;
		status.sweep_ms = 0;
		status.sweep_ms_meas = 14.2;
		updateStatus(status);
		// The info bar throttles display writes; force the next repaint without waiting.
		const clock = vi.spyOn(performance, 'now').mockReturnValue(performance.now() + 2000);
		updateInfoBar();
		expect(document.getElementById('info-swt')?.textContent).toContain('14.2 ms');
		status.sweep_ms = 12.5;
		status.sweep_ms_meas = 999;
		updateStatus(status);
		clock.mockReturnValue(performance.now() + 4000);
		updateInfoBar();
		expect(document.getElementById('info-swt')?.textContent).toBe('12.5 ms');
		clock.mockRestore();
		document.body.innerHTML = '';
	});
});

describe('SDR squelch zero threshold', () => {
	it('keeps zero as a real slider setting', async () => {
		const { bindActions } = await import('../ui/controls');
		document.body.innerHTML = '<input type="range" id="input-sdr-squelch" min="-120" max="0" value="-110">';
		bindActions();
		const input = document.getElementById('input-sdr-squelch') as HTMLInputElement;
		input.value = '0';
		input.dispatchEvent(new Event('change'));
		expect(sdrSquelch.get()).toBe(0);
		document.body.innerHTML = '';
	});
});

describe('a reference level the device clamped to its own range', () => {
	// Asked for explicitly: requesting a level the current front end cannot program used to make
	// the canvas announce "Device limited Ref to X dBm" and to grey the step arrows at the ends
	// of the range. Both are gone - the echoed level is simply the level on screen, and a press
	// past the end is a silent no-op. The pairs are only test data: at runtime the value comes
	// from `actual.ref` in the STATUS, which is why several pairs are checked.
	const limited = [[30, 27], [30, 25], [25, 22], [-5, -8]] as const;

	it.each(limited)('stays silent when the device programmed %i as %i', (asked, reported) => {
		const status = structuredClone(SWP_STATUS) as any;
		status.req.swp.ref = asked;
		status.actual.ref = reported;
		status.ref = reported;
		S.setNoticeText('');
		updateStatus(status);
		expect(noticeText).toBe('');                  // nothing posted, not even once
		expect(refLevel.get()).toBe(reported);        // ...but the echoed level is followed
	});

	it('never greys the step arrows at the ends of the range', () => {
		document.body.innerHTML =
			'<input id="input-ref"><button id="btn-ref-set"></button>' +
			'<button id="btn-ref-down"></button><button id="btn-ref-up"></button>';
		const at = (level: number) => {
			const status = structuredClone(SWP_STATUS) as any;
			status.ref = level;
			status.req.swp.ref = level;
			status.actual.ref = level;
			updateStatus(status);
			expect((document.getElementById('btn-ref-down') as HTMLButtonElement).disabled).toBe(false);
			expect((document.getElementById('btn-ref-up') as HTMLButtonElement).disabled).toBe(false);
		};
		at(-50);                                       // the swept low end
		at(30);                                        // the swept high end
		document.body.innerHTML = '';
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
	it('repaints the primary input meter on consecutive STATUS and clears missing measurements', () => {
		const meter = document.createElement('span');
		meter.id = 'cur-sdr-level';
		document.body.append(meter);
		const status = structuredClone(SDR_STATUS) as Record<string, any>;
		status.sdr.channels = [{ channel: 1, enabled: true, level_dbfs: -6.0206 }];
		meter.title = 'old IQ error';
		updateStatus(status);
		expect(meter.textContent).toBe('-6.0 dBFS');
		expect(meter.hasAttribute('title')).toBe(false);
		status.sdr.channels[0].level_dbfs = -26.0206;
		updateStatus(status);
		expect(meter.textContent).toBe('-26.0 dBFS');
		status.sdr.channels[0].level_dbfs = null;
		updateStatus(status);
		expect(meter.textContent).toBe('');
		delete status.sdr.channels;
		status.sdr.level_dbfs = null;
		updateStatus(status);
		expect(meter.textContent).toBe('');
		meter.remove();
	});
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

	it('forwards the confirmed channel registry without driving IQ stream identity', () => {
		const status = structuredClone(SDR_STATUS) as Record<string, any>;
		status.actual.channel_registry = [{
			channel: 0, channel_id: 2, epoch: 7, frequency_hz: 101762500,
			output_rate: 48000, input_rate: 96000, group_delay_samples: 63.5, state: 'active',
		}];
		updateStatus(status);
		expect(sdrIqStats()).toMatchObject({
			confirmedRegistry: status.actual.channel_registry,
			streamKey: '',        // STATUS never selects a stream; the frame header does
		});
		status.actual.channel_registry = undefined;
		updateStatus(status);
		expect(sdrIqStats().confirmedRegistry).toBeNull();
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

describe('waterfall table replacement during STATUS', () => {
	afterEach(() => {
		setWaterfall(false);
		S.setMeasOn(false);
		updateStatus(structuredClone(SWP_STATUS));
		document.body.innerHTML = '';
	});

	it.each(['std', 'rta', 'sdr'])('keeps the marker table out of the %s layout on repeated reports', mode => {
		document.body.innerHTML = '<button id="btn-waterfall"></button>'
			+ '<table id="marker-table"></table><div id="waterfall-container"></div><div id="wf-split"></div>';
		S.setMeasOn(false);
		const report = { ...structuredClone(mode === 'sdr' ? SDR_STATUS : SWP_STATUS), mode };
		updateStatus(report);
		setWaterfall(true);
		const table = document.getElementById('marker-table')!;
		for (let n = 0; n < 3; n++) {
			updateStatus(structuredClone(report));
			expect(table.style.display).toBe('none');
		}
		setWaterfall(false);
		updateStatus(structuredClone(report));
		expect(table.style.display).toBe('');
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
