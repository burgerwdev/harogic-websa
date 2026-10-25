/**
 * Per-mode tuning steps.
 *
 * The three tiers (normal / coarse / fine) are remembered per demodulation mode and default
 * to fixed, mode-appropriate values that do not follow the capture width. These tests pin
 * the rules the tune keys rely on: the defaults, the per-mode memory, the persistence and
 * the Preset reset.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { resetAll } from '../core/params';
import { UNIT_OPTIONS, toUnit, units } from '../core/units';
import {
	SDR_STEP_QUICK_HZ, SDR_PREF_KEYS, currentSdrStepHz, effectiveLocateMode, parseSdrStepMap,
	resetSdrState, sdrChannelAnchorHz, sdrChannelStepHz, sdrDemod,
	sdrFineStepByMode, sdrLocateMode, sdrNormalStepByMode, setSdrStepForCurrentMode,
} from '../ui/sdrState';
import { signalLocateSupported, stepTierDefaults } from '../ui/sdrTuning';

beforeEach(() => {
	localStorage.clear();
	resetAll('sdr');
});

describe('per-mode tier defaults', () => {
	it('gives every voice mode its band-plan ladder', () => {
		expect(stepTierDefaults('am')).toEqual({ normal: 1000, coarse: 5000, fine: 100 });
		expect(stepTierDefaults('dsb')).toEqual({ normal: 1000, coarse: 5000, fine: 100 });
		expect(stepTierDefaults('usb')).toEqual({ normal: 100, coarse: 1000, fine: 10 });
		expect(stepTierDefaults('lsb')).toEqual({ normal: 100, coarse: 1000, fine: 10 });
		expect(stepTierDefaults('cw')).toEqual({ normal: 10, coarse: 100, fine: 1 });
		expect(stepTierDefaults('nfm')).toEqual({ normal: 12500, coarse: 100000, fine: 1000 });
		expect(stepTierDefaults('pm')).toEqual({ normal: 12500, coarse: 100000, fine: 1000 });
		expect(stepTierDefaults('wfm')).toEqual({ normal: 100000, coarse: 1000000, fine: 10000 });
	});

	it('keeps protocol modes on conservative dial-accuracy steps', () => {
		expect(stepTierDefaults('ft8')).toEqual({ normal: 10, coarse: 100, fine: 1 });
		expect(stepTierDefaults('sstv')).toEqual({ normal: 1000, coarse: 10000, fine: 100 });
		expect(stepTierDefaults('drm')).toEqual({ normal: 1000, coarse: 10000, fine: 100 });
		expect(stepTierDefaults('drmplus')).toEqual({ normal: 10000, coarse: 100000, fine: 1000 });
	});

	it('serves a conservative generic ladder for an unknown mode', () => {
		expect(stepTierDefaults('nope')).toEqual({ normal: 1000, coarse: 10000, fine: 100 });
	});

	it('serves the defaults whatever the capture width: the step no longer follows the span', () => {
		// Same mode, two very different spans: the default tiers are identical. The old
		// per-bandwidth step made a 50 MHz sweep jump to 500 kHz and hid narrow signals.
		const wide = stepTierDefaults('am');
		expect(wide).toEqual(stepTierDefaults('am'));
		expect(currentSdrStepHz()).toBe(1000); // am default, no decimate involvement
	});
});

describe('per-mode tier memory', () => {
	it('keeps one step per mode and tier and returns each mode its own', () => {
		sdrDemod.set('am');
		setSdrStepForCurrentMode(2500);
		setSdrStepForCurrentMode(10000, 'coarse');
		setSdrStepForCurrentMode(25, 'fine');
		expect(currentSdrStepHz()).toBe(2500);
		expect(currentSdrStepHz('coarse')).toBe(10000);
		expect(currentSdrStepHz('fine')).toBe(25);

		sdrDemod.set('cw'); // no stored step: cw's own ladder
		expect(currentSdrStepHz()).toBe(10);
		expect(currentSdrStepHz('coarse')).toBe(100);
		expect(currentSdrStepHz('fine')).toBe(1);

		sdrDemod.set('am'); // the am ladder came back unchanged
		expect(currentSdrStepHz()).toBe(2500);
		expect(currentSdrStepHz('coarse')).toBe(10000);
		expect(currentSdrStepHz('fine')).toBe(25);
	});

	it('ignores an impossible step', () => {
		sdrDemod.set('am');
		setSdrStepForCurrentMode(0);
		setSdrStepForCurrentMode(-100);
		setSdrStepForCurrentMode(NaN);
		setSdrStepForCurrentMode(0.5, 'fine'); // below the 1 Hz floor
		expect(sdrNormalStepByMode.get()).toEqual({});
		expect(sdrFineStepByMode.get()).toEqual({});
		expect(currentSdrStepHz()).toBe(1000);
	});

	it('persists per tier and Preset clears all three with the old bandwidth key', () => {
		sdrDemod.set('am');
		setSdrStepForCurrentMode(2500);
		setSdrStepForCurrentMode(20000, 'coarse');
		setSdrStepForCurrentMode(50, 'fine');
		expect(localStorage.getItem('web-sa-sdr-step-normal')).toBe('{"am":2500}');
		expect(localStorage.getItem('web-sa-sdr-step-coarse')).toBe('{"am":20000}');
		expect(localStorage.getItem('web-sa-sdr-step-fine')).toBe('{"am":50}');

		resetSdrState();
		expect(sdrNormalStepByMode.get()).toEqual({});
		expect(currentSdrStepHz()).toBe(1000);
		for (const key of ['web-sa-sdr-step-normal', 'web-sa-sdr-step-coarse', 'web-sa-sdr-step-fine']) {
			expect(localStorage.getItem(key)).toBeNull();
		}
	});

	it('clears the retired per-bandwidth step key so it cannot survive a Preset', () => {
		localStorage.setItem('web-sa-sdr-step-bw', '{"256":2500}');
		resetSdrState();
		expect(localStorage.getItem('web-sa-sdr-step-bw')).toBeNull();
	});
});

describe('the stored step slot', () => {
	it('parses a stored map and drops entries below 1 Hz or not finite', () => {
		const parsed = parseSdrStepMap(
			JSON.stringify({ am: 2500, cw: 0.5, usb: -1, lsb: 'x', nfm: NaN, wfm: Infinity }));
		expect(parsed).toEqual({ am: 2500 });
	});

	it('degrades corrupt storage to an empty map', () => {
		expect(parseSdrStepMap('{not json')).toEqual({});
		expect(parseSdrStepMap('null')).toEqual({});
		expect(parseSdrStepMap('')).toEqual({});
	});
});

describe('step unit and quick steps', () => {
	it('offers the quick steps in Hz and the shared unit buttons reach 1 Hz', () => {
		expect(SDR_STEP_QUICK_HZ).toEqual([
			1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000,
		]);
		expect(UNIT_OPTIONS.sdr_step).toEqual(['Hz', 'kHz', 'MHz']);
		expect(units().sdr_step).toBe('kHz');
		expect(toUnit(1000, 'sdr_step')).toBe(1);
		expect(toUnit(1, 'sdr_step')).toBe(0.001);
	});
});

describe('positioning preferences', () => {
	it('offers signal locate only for AM and CW', () => {
		expect(signalLocateSupported('am')).toBe(true);
		expect(signalLocateSupported('cw')).toBe(true);
		for (const mode of ['usb', 'lsb', 'dsb', 'nfm', 'wfm', 'pm', 'ft8', 'sstv', 'drm', 'drmplus']) {
			expect(signalLocateSupported(mode)).toBe(false);
		}
	});

	it('keeps a signal preference but serves free while the mode cannot use it', () => {
		sdrLocateMode.set('signal');
		sdrDemod.set('usb');
		expect(effectiveLocateMode('usb')).toBe('free');
		expect(sdrLocateMode.get()).toBe('signal'); // remembered for the return
		sdrDemod.set('am');
		expect(effectiveLocateMode('am')).toBe('signal');
	});

	it('resets locate and grid preferences with Preset', () => {
		sdrLocateMode.set('channel');
		sdrChannelStepHz.set(12500);
		sdrChannelAnchorHz.set(100000);
		resetSdrState();
		expect(sdrLocateMode.get()).toBe('free');
		expect(sdrChannelStepHz.get()).toBe(25000);
		expect(sdrChannelAnchorHz.get()).toBe(0);
		for (const key of SDR_PREF_KEYS) expect(localStorage.getItem(key)).toBeNull();
	});

	it('stores fractional spacing from 1 Hz up and a nonzero anchor', () => {
		sdrChannelStepHz.set(12.5); // kHz-plans with a fraction, still >= 1 Hz
		sdrChannelAnchorHz.set(156.25);
		expect(sdrChannelStepHz.get()).toBe(12.5);
		expect(sdrChannelAnchorHz.get()).toBe(156.25);
		// A stored spacing below the floor degrades to the default, not to the raw value.
		localStorage.setItem('web-sa-sdr-channel-step', '0.2');
	});
});
