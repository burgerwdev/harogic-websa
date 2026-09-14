/**
 * Auto Scale: the one-shot reference placement and its feedback.
 *
 * Two things are asserted here that the old continuous implementation got wrong: pressing Auto
 * must produce exactly one fit (not a tracking mode), and the button must show that something
 * is happening while the device reconfigures.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getDisplayRef, setDisplayRef } from '../ui/displayRef';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { setWS } from '../core/wsSend';
import {
	autoScaleBusy,
	autoScaleRequest,
	clearAutoScaleHint,
	maybeRequestSdrFit,
	requestSdrEntryFit,
	resetAutoScaleState,
	syncAutoScaleStatus,
} from '../ui/refAutoScale';
import { refLevel } from '../ui/refState';

const sent: any[] = [];

function openSocket() {
	sent.length = 0;
	setWS({ readyState: WebSocket.OPEN, send: (m: string) => sent.push(JSON.parse(m)) } as any);
}

function status(auto_ref: Record<string, unknown>) {
	syncAutoScaleStatus({ auto_ref });
}

beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	resetAutoScaleState();
	resetGraphMode();
	setDisplayRef('preset', 0);
	openSocket();
	refLevel.set(0);
	S.setDbPerDiv(10);
});

describe('SWP/RTA Auto Scale', () => {
	it('asks the backend for one fit and carries the window height', () => {
		autoScaleRequest();
		expect(sent).toEqual([{
			cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv,
			current_ref: getDisplayRef(),          // the level the user is looking at
		}]);
		expect(autoScaleBusy()).toBe(true);
	});

	it('stops glowing when the backend reports that the fit landed, and names the result', () => {
		autoScaleRequest();
		const hint = document.createElement('span');
		hint.id = 'ref-hint';
		document.body.appendChild(hint);
		// A decision is "new" when its sequence number moves: a sticky old result must not clear
		// the glow while the press is still in flight.
		status({ adjusting: false, result: 'applied', target: -10, seq: 1 });   // the old decision
		autoScaleRequest();
		status({ adjusting: false, result: 'applied', target: -10, seq: 1 });   // still sticky
		expect(autoScaleBusy()).toBe(true);
		status({ adjusting: false, result: 'applied', target: -10, seq: 2 });   // the new answer
		expect(autoScaleBusy()).toBe(false);
		expect(hint.textContent).toBe('Ref \u2192 -10 dBm');
	});

	it('announces a level whenever one was applied, including a safety correction', () => {
		const hint = document.createElement('span');
		hint.id = 'ref-hint';
		document.body.appendChild(hint);
		status({ adjusting: false, result: 'out_of_window', target: -35, seq: 3 });
		expect(hint.textContent).toBe('Ref \u2192 -35 dBm');
	});

	it('explains a refusal instead of doing nothing', () => {
		const hint = document.createElement('span');
		hint.id = 'ref-hint';
		document.body.appendChild(hint);
		status({ adjusting: false, result: 'no_signal', seq: 1 });
		expect(hint.textContent).toBe('No signal to fit');
	});

	it('keeps a repeated message visible for its own hold time', () => {
		vi.useFakeTimers();
		const el = document.createElement('span');
		el.id = 'ref-hint';
		document.body.appendChild(el);
		status({ adjusting: false, result: 'no_signal', seq: 1 });
		vi.advanceTimersByTime(4000);
		status({ adjusting: false, result: 'no_signal', seq: 2 });   // the same message again
		vi.advanceTimersByTime(3000);                               // past the FIRST hold
		expect(el.textContent).toBe('No signal to fit');            // the newest hold owns it
		vi.advanceTimersByTime(3100);
		expect(el.textContent).toBe('');
		vi.useRealTimers();
	});

	it('keeps glowing while the backend is still adjusting', () => {
		const btn = document.createElement('button');
		btn.id = 'btn-ref-auto';
		document.body.appendChild(btn);
		autoScaleRequest();
		syncAutoScaleStatus({ auto_ref: { adjusting: true, result: 'applied', seq: 1 } });
		expect(btn.classList.contains('busy')).toBe(true);
		expect(autoScaleBusy()).toBe(true);
		// A status that does not belong to a press must not start a glow of its own.
		clearAutoScaleHint();
		syncAutoScaleStatus({ auto_ref: { adjusting: true, result: 'applied', seq: 2 } });
		expect(btn.classList.contains('busy')).toBe(false);
	});

	it('drops the glow after its fallback timeout even if no status arrives', () => {
		vi.useFakeTimers();
		autoScaleRequest();
		expect(autoScaleBusy()).toBe(true);
		vi.advanceTimersByTime(4100);
		expect(autoScaleBusy()).toBe(false);
		vi.useRealTimers();
	});
});

describe('SDR Auto Scale', () => {
	it('uses the same command as the other modes, and follows the reported target', () => {
		graphMode.confirm('sdr');
		// Entering SDR asks the backend as soon as frames arrive.
		requestSdrEntryFit();
		maybeRequestSdrFit();
		expect(sent.filter(m => m.cmd === 'AUTO_SCALE').length).toBe(1);

		// The backend fits and reports the target: the display scale follows it once.
		syncAutoScaleStatus({
			auto_ref: { adjusting: false, result: 'applied', target: -15, seq: 1 },
		});
		expect(getDisplayRef()).toBe(-15);
		// A sticky result must not overwrite a later manual Ref.
		clearAutoScaleHint();
		setDisplayRef('user', -40);
		syncAutoScaleStatus({
			auto_ref: { adjusting: false, result: 'applied', target: -15, seq: 1 },
		});
		expect(getDisplayRef()).toBe(-40);
	});

	it('does not let an autonomous correction move a level the user just set', () => {
		graphMode.confirm('sdr');
		setDisplayRef('preset', -40);
		clearAutoScaleHint();                    // a manual Ref edit takes the scale over
		syncAutoScaleStatus({
			auto_ref: { adjusting: false, result: 'out_of_window', target: -10, seq: 5 },
		});
		expect(getDisplayRef()).toBe(-40);
		// An explicit press claims it back.
		autoScaleRequest();
		syncAutoScaleStatus({
			auto_ref: { adjusting: false, result: 'applied', target: -10, seq: 6 },
		});
		expect(getDisplayRef()).toBe(-10);
	});

	it('does not write the display for a refusal', () => {
		graphMode.confirm('sdr');
		setDisplayRef('preset', -40);
		syncAutoScaleStatus({
			auto_ref: { adjusting: false, result: 'no_signal', target: null, seq: 1 },
		});
		expect(getDisplayRef()).toBe(-40);
	});
});
