/**
 * Peak-list threshold: one decision per measurement geometry.
 *
 * It used to be recomputed every frame from the instantaneous global peak, so a fast amplitude
 * change moved the threshold with it - and the threshold decides peak-table membership and
 * marker peak search, so both reshuffled. What must hold now: the value is fitted once per
 * geometry from a robust estimate, a signal swing alone never moves it, and a manual edit still
 * owns it until Auto is pressed again.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { autoPeakThr, peakThrAuto, peakThrManual, resetPeakThr } from '../render/peaklist';
import { peakThr, peakThrUserSet } from '../ui/measurePrefs';
import { currentRBW, rbwMode } from '../ui/swpState';
import { resetAll } from '../core/params';
import { fmtAxisLevel, fmtReadoutLevel } from '../core/level';
import { displayOffset, displayUnit } from '../ui/displayState';
import * as S from '../core/store';

function trace(peakDbm: number): Float32Array {
	const spec = new Float32Array(1000).fill(-100);
	for (let i = 490; i < 510; i++) spec[i] = peakDbm;
	return spec;
}

function withMarkers() {
	S.markers.forEach((m, i) => { m.enabled = i === 0; });
}

beforeEach(() => {
	document.body.replaceChildren();
	const el = document.createElement('input');
	el.id = 'input-peakthr';
	el.value = '-80';
	document.body.appendChild(el);
	resetAll();
	resetPeakThr();                  // the auto value is latched module-side: start clean
	withMarkers();
	rbwMode.confirm('manual');
	currentRBW.confirm(100e3);
	S.traces[0].powers = trace(-20);
	S.traces[0].isNormalized = false;
	S.traces[0].reference = null;
});

describe('auto peak threshold', () => {
	it('fits once for a geometry and then ignores signal swings', () => {
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-20));
		const fitted = peakThr.get();
		expect(fitted).toBe(-70);                     // robust peak -20 minus 50 dB
		// The signal jumps 30 dB: the threshold must not follow it.
		for (let i = 0; i < 10; i++) autoPeakThr(trace(10));
		expect(peakThr.get()).toBe(fitted);
	});

	it('re-fits when the measurement geometry changes', () => {
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-20));
		expect(peakThr.get()).toBe(-70);
		currentRBW.confirm(300e3);                    // a new RBW moves the noise floor
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-40));
		expect(peakThr.get()).toBe(-90);
	});

	it('is not moved by a single-bin spike', () => {
		const spiky = new Float32Array(1000).fill(-95);
		spiky[123] = 20;                              // one bin, one frame
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-30));
		expect(peakThr.get()).toBe(-80);
		autoPeakThr(spiky);
		expect(peakThr.get()).toBe(-80);
	});

	it('does nothing while a marker is off, or after a manual edit', () => {
		S.markers.forEach(m => { m.enabled = false; });
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-10));
		expect(peakThr.get()).toBe(-80);              // untouched fallback
		withMarkers();

		const el = document.getElementById('input-peakthr') as HTMLInputElement;
		el.value = '-33';
		peakThrManual();
		expect(peakThr.get()).toBe(-33);
		expect(peakThrUserSet.get()).toBe(true);
		for (let i = 0; i < 10; i++) autoPeakThr(trace(-10));
		expect(peakThr.get()).toBe(-33);              // the user owns the value
	});

	it('fits immediately when Auto is pressed again', () => {
		const el = document.getElementById('input-peakthr') as HTMLInputElement;
		el.value = '-33';
		peakThrManual();
		expect(peakThr.get()).toBe(-33);
		S.traces[0].powers = trace(-10);         // a louder trace is on screen now
		peakThrAuto();
		expect(peakThrUserSet.get()).toBe(false);
		expect(peakThr.get()).toBe(-60);         // -10 dBm peak, 50 dB below
		expect(el.value).toBe('-60');
	});

	it('projects the value into the input, and Preset returns to automatic', () => {
		for (let i = 0; i < 6; i++) autoPeakThr(trace(-20));
		const el = document.getElementById('input-peakthr') as HTMLInputElement;
		expect(el.value).toBe('-70');
		resetPeakThr();
		expect(peakThr.get()).toBe(-80);
		expect(el.value).toBe('-80');
		expect(peakThrUserSet.get()).toBe(false);
	});
});

describe('absolute readouts follow the display rule', () => {
	it('applies the external offset to the axis and the readouts', () => {
		displayUnit.set('dBm');
		displayOffset.set(0);
		expect(fmtAxisLevel(-20)).toBe('-20');
		expect(fmtReadoutLevel(-25.5)).toBe('-25.50 dBm');
		displayOffset.set(20);                      // a 20 dB amplifier in front of the analyser
		expect(fmtAxisLevel(-20)).toBe('0');        // the top line now means 0 dBm at the antenna
		expect(fmtReadoutLevel(-25.5)).toBe('-5.50 dBm');
	});

	it('converts the unit and leaves relative values alone', () => {
		displayOffset.set(0);
		displayUnit.set('dBm');
		expect(fmtAxisLevel(0)).toBe('0');
		expect(fmtReadoutLevel(0)).toBe('0.00 dBm');
		displayUnit.set('dB');
		displayOffset.set(20);                      // differences are never converted
		expect(fmtAxisLevel(-20)).toBe('-20');
		expect(fmtReadoutLevel(-25.5)).toBe('-25.50 dB');
	});
});
