/**
 * Auto Scale: the one-shot reference placement and its feedback.
 *
 * Two things are asserted here that the old continuous implementation got wrong: pressing Auto
 * must produce exactly one fit (not a tracking mode), and the button must show that something
 * is happening while the device reconfigures.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getDisplayRef, setDisplayRef } from '../ui/displayRef';
import { resetGraphMode } from '../ui/graphMode';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { setWS } from '../core/wsSend';
import {
	autoScaleBusy,
	autoScaleRequest,
	clearAutoScaleHint,
	maybeFitSdrFrame,
	requestSdrEntryFit,
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
	resetGraphMode();
	setDisplayRef('preset', 0);
	openSocket();
	refLevel.set(0);
	S.setDbPerDiv(10);
});

describe('SWP/RTA Auto Scale', () => {
	it('asks the backend for one fit and carries the window height', () => {
		autoScaleRequest();
		expect(sent).toEqual([{ cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv }]);
		expect(autoScaleBusy()).toBe(true);
	});

	it('stops glowing when the backend reports that the fit landed, and names the result', () => {
		autoScaleRequest();
		const hint = document.createElement('span');
		hint.id = 'ref-hint';
		document.body.appendChild(hint);
		status({ adjusting: false, result: 'applied', target: -10 });
		expect(autoScaleBusy()).toBe(false);
		expect(hint.textContent).toBe('Ref \u2192 -10 dBm');
	});

	it('explains a refusal instead of doing nothing', () => {
		const hint = document.createElement('span');
		hint.id = 'ref-hint';
		document.body.appendChild(hint);
		status({ adjusting: false, result: 'no_signal' });
		expect(hint.textContent).toBe('No signal to fit');
	});

	it('keeps glowing while the backend is still adjusting', () => {
		const btn = document.createElement('button');
		btn.id = 'btn-ref-auto';
		document.body.appendChild(btn);
		autoScaleRequest();
		syncAutoScaleStatus({ auto_ref: { adjusting: true, result: 'applied' } });
		expect(btn.classList.contains('busy')).toBe(true);
		expect(autoScaleBusy()).toBe(true);
		// A status that does not belong to a press must not start a glow of its own.
		clearAutoScaleHint();
		syncAutoScaleStatus({ auto_ref: { adjusting: true, result: 'applied' } });
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
	it('fits once on request and then leaves the display alone', () => {
		resetGraphMode();
		// Entering SDR asks for the fit; the first plausible frame answers it.
		requestSdrEntryFit();
		const frame = new Float32Array(1000).fill(-100);
		for (let i = 480; i < 520; i++) frame[i] = -20;
		maybeFitSdrFrame(frame);
		const fitted = getDisplayRef();
		expect(fitted).toBeGreaterThan(-160);      // the trace was placed in the window

		// A later frame with a much louder signal must NOT move the display by itself.
		const louder = new Float32Array(1000).fill(-60);
		maybeFitSdrFrame(louder);
		expect(getDisplayRef()).toBe(fitted);
	});

	it('leaves a signal-free frame alone', () => {
		requestSdrEntryFit();
		maybeFitSdrFrame(new Float32Array(1000).fill(-200));
		expect(getDisplayRef()).toBe(0);
	});

	it('writes the device level only when it is off, so the audio is not interrupted', () => {
		requestSdrEntryFit();
		const frame = new Float32Array(1000).fill(-100);
		for (let i = 480; i < 520; i++) frame[i] = -20;
		refLevel.set(0);                            // the device is already within 3 dB
		maybeFitSdrFrame(frame);
		const settled = getDisplayRef();
		refLevel.set(settled);
		requestSdrEntryFit();
		maybeFitSdrFrame(frame);
		// 'SET_REF' only appears for a level the device does not already have.
		expect(sent.filter(m => m.cmd === 'SET_REF').length).toBeLessThanOrEqual(1);
	});

	it('a manual Ref edit ends the pending fit', () => {
		requestSdrEntryFit();
		clearAutoScaleHint();
		const frame = new Float32Array(1000).fill(-100);
		for (let i = 480; i < 520; i++) frame[i] = -20;
		maybeFitSdrFrame(frame);
		expect(getDisplayRef()).toBe(0);
	});
});
