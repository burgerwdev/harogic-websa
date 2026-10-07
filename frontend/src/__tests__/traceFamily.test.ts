/**
 * The trace mode each display family remembers.
 *
 * SDR is a panadapter (an average is what smooths its noise floor) and the swept/RTA views are a
 * sweeper (Clear Write), and the trace mode is per-trace in-memory state that is not persisted
 * anywhere - so without this the SDR entry always started on Clear Write and a trip to RTA threw
 * the user's choice away. What is pinned here: the SDR default (Average at the panel's own default
 * depth), that a gesture inside SDR survives leaving and re-entering, and that leaving SDR restores
 * the swept side unchanged.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { updateStatus } from '../core/ws';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import {
	SDR_DEFAULT_AVG,
	applyTraceFamilyMode,
	rememberTraceMode,
	setTraceMode,
	switchTraceTab,
	syncAvgUI,
} from '../ui/traceOps';

function traceSelect(mode: string): HTMLSelectElement {
	document.body.innerHTML =
		'<select id="select-trace-mode">' +
		`<option value="CLEAR_WRITE"${mode === 'CLEAR_WRITE' ? ' selected' : ''}>Clear Write</option>` +
		`<option value="MAX_HOLD"${mode === 'MAX_HOLD' ? ' selected' : ''}>Max Hold</option>` +
		`<option value="AVERAGE"${mode === 'AVERAGE' ? ' selected' : ''}>Average</option>` +
		'</select><div id="trace-avg-row" style="display:none;"></div>' +
		'<select id="select-trace-avg"><option value="16" selected>16</option>' +
		'<option value="32">32</option></select><span id="trace-avg-status"></span>';
	return document.getElementById('select-trace-mode') as HTMLSelectElement;
}

const activeTrace = () => S.traces[S.activeTraceIdx];

/**
 * A deterministic starting point.
 *
 * `resetAll()` deliberately keeps CONFIRMED values (only the intent is reset), so the confirmed
 * mode is set explicitly here; the trace mode and the per-family memory are then staged back to
 * their factory state - `rememberTraceMode` records whatever the trace shows, so each family's
 * default has to be on screen while it is recorded.
 */
beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	resetGraphMode();
	graphMode.confirm('std');
	traceSelect('CLEAR_WRITE');
	S.setActiveTraceIdx(0);
	S.traces.forEach((trace, i) => { trace.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; });
	S.traces[0].mode = 'CLEAR_WRITE';
	rememberTraceMode(false);
	S.traces[0].mode = 'AVERAGE';
	rememberTraceMode(true);
	S.traces[0].mode = 'CLEAR_WRITE';
});

describe('entering a display family', () => {
	it('opens SDR on an Average at the default depth', () => {
		applyTraceFamilyMode(true);
		expect(activeTrace().mode).toBe('AVERAGE');
		expect(activeTrace().avgTargetRta).toBe(SDR_DEFAULT_AVG);
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value)
			.toBe('AVERAGE');
		expect((document.getElementById('trace-avg-row') as HTMLElement).style.display).toBe('');
	});

	it('leaves the swept side on the factory Clear Write', () => {
		applyTraceFamilyMode(false);
		expect(activeTrace().mode).toBe('CLEAR_WRITE');
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value)
			.toBe('CLEAR_WRITE');
	});

	it('only touches the active trace', () => {
		S.setActiveTraceIdx(2);
		applyTraceFamilyMode(true);
		expect(S.traces[2].mode).toBe('AVERAGE');
		expect(S.traces[0].mode).toBe('CLEAR_WRITE');
		S.setActiveTraceIdx(0);
	});
});

describe('a choice made inside a family', () => {
	it('survives leaving SDR and coming back', () => {
		applyTraceFamilyMode(true);              // enter SDR: Average/16
		setTraceMode('MAX_HOLD');                // the user changes their mind
		expect(activeTrace().mode).toBe('MAX_HOLD');

		rememberTraceMode(true);                 // leave SDR for RTA
		applyTraceFamilyMode(false);
		expect(activeTrace().mode).toBe('CLEAR_WRITE');

		rememberTraceMode(false);                // leave RTA for SDR again
		applyTraceFamilyMode(true);
		expect(activeTrace().mode).toBe('MAX_HOLD');   // not reset to the SDR default
	});

	it('does not let the swept side inherit it', () => {
		applyTraceFamilyMode(true);
		setTraceMode('MAX_HOLD');
		rememberTraceMode(true);
		applyTraceFamilyMode(false);
		expect(activeTrace().mode).toBe('CLEAR_WRITE');
	});
});

describe('the STATUS wiring', () => {
	/** The fields syncGraphModeStatus needs; the rest of updateStatus guards its own. */
	const status = (mode: string) => ({
		cmd: 'STATUS', connected: true, mode,
		center: 100200000, span: 1562500, ref: -20,
		points: 100, window: 1, detector: 'auto', sweep_time_mode: 0,
		caps: { model: 67, name: 'SAN-90', fmin: 8000, fmax: 9020000000, ref_min: -50, ref_max: 30 },
		req: { center: 100200000, span: 1562500, points: 100, swp: { ref: -20 }, rta: {} },
		actual: { ref: -20 },
	});

	it('applies the SDR default on a confirmed mode change only', () => {
		updateStatus(status('sdr'));
		expect(activeTrace().mode).toBe('AVERAGE');
		expect(activeTrace().avgTargetRta).toBe(SDR_DEFAULT_AVG);
		// The user's change is not undone by every following STATUS frame.
		setTraceMode('MIN_HOLD');
		updateStatus(status('sdr'));
		updateStatus(status('sdr'));
		expect(activeTrace().mode).toBe('MIN_HOLD');
	});

	it('restores the swept mode and depth on the way back', () => {
		updateStatus(status('std'));
		applyTraceFamilyMode(false);
		const before = activeTrace().mode;
		updateStatus(status('sdr'));
		expect(activeTrace().mode).toBe('AVERAGE');
		updateStatus(status('std'));
		expect(activeTrace().mode).toBe(before);
	});

	it('keeps the dropdown and the Avg row in step with the trace', () => {
		updateStatus(status('sdr'));
		syncAvgUI();
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value)
			.toBe('AVERAGE');
		expect((document.getElementById('select-trace-avg') as HTMLSelectElement).value)
			.toBe(String(SDR_DEFAULT_AVG));
		switchTraceTab(0);
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value)
			.toBe('AVERAGE');
	});
});
