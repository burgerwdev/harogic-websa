import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { prepareSdrAudioTransition } from '../audio/sdrAudio';
import { resetAll } from '../core/params';
import * as S from '../core/store';
import { send, setWS } from '../core/wsSend';
import { getY, plotRect } from '../render/plot';
import { getDisplayRef, noteDisplayRefReport, pendingDisplayRef, setDisplayRef } from '../ui/displayRef';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import { commitLevelOffset } from '../ui/panels/refAmp';
import {
	autoScaleBusy, autoScaleRequest, clearAutoScaleHint, maybeRequestSdrFit,
	noteSdrRefState, noteTraceObservation, requestSdrEntryFit, resetAutoScaleState,
	syncAutoScaleStatus,
} from '../ui/refAutoScale';
import { refLevel } from '../ui/refState';

vi.mock('../audio/sdrAudio', () => ({ prepareSdrAudioTransition: vi.fn() }));

const sent: Record<string, unknown>[] = [];

function status(auto_ref: Parameters<typeof syncAutoScaleStatus>[0]['auto_ref']) {
	syncAutoScaleStatus({ auto_ref });
}

function acknowledge(seq: number, hardwareAdjusting = false) {
	const request = sent.filter(command => command.cmd === 'AUTO_SCALE').at(-1);
	syncAutoScaleStatus({ response_to: 'AUTO_SCALE', request_id: request?.request_id,
		auto_ref: { result: 'adjusting', adjusting: true, seq, hardware_adjusting: hardwareAdjusting } });
}

beforeEach(() => {
	vi.useFakeTimers();
	vi.clearAllMocks();
	document.body.replaceChildren();
	const button = document.createElement('button');
	button.id = 'btn-ref-auto';
	document.body.appendChild(button);
	resetAll();
	resetAutoScaleState();
	resetGraphMode();
	setDisplayRef('preset', 0);
	S.setNoticeText(null);
	S.setDbPerDiv(10);
	sent.length = 0;
	// The socket double only implements the surface used by wsSend.
	setWS({ readyState: WebSocket.OPEN, send: (message: string) => sent.push(JSON.parse(message)) } as unknown as WebSocket);
});

afterEach(() => {
	setWS(null);
	vi.restoreAllMocks();
	vi.clearAllTimers();
	vi.useRealTimers();
});
describe('Auto request scope and takeover', () => {
	it('authorizes normal user Auto in every mode and sends the visible half-dB scale', () => {
		S.setDbPerDiv(0.5);
		for (const mode of ['std', 'rta', 'sdr'] as const) {
			graphMode.confirm(mode);
			autoScaleRequest();
			const command = sent.at(-1);
			expect(command).toMatchObject({ cmd: 'AUTO_SCALE', range_db: S.totalDivs * 0.5, current_ref: 0 });
			expect(command?.display_only).toBeUndefined();
		}
	});

	it('makes SDR entry display-only and never prepares an IQS audio transition', () => {
		graphMode.confirm('sdr');
		syncAutoScaleStatus({ status_warning: -12 });
		requestSdrEntryFit();
		noteSdrRefState({ ref: 0, ref_set: false });
		maybeRequestSdrFit();
		maybeRequestSdrFit();
		expect(sent).toHaveLength(1);
		expect(sent[0]).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true });
		acknowledge(1);
		status({ result: 'overflow', target: null, adjusting: false, seq: 2 });
		expect(prepareSdrAudioTransition).not.toHaveBeenCalled();
		expect(getDisplayRef()).toBe(0);
	});

	it.each([0, 60])('preserves an SDR hardware level on entry with offset %s without starting a fit', offset => {
		graphMode.confirm('sdr');
		commitLevelOffset(offset);
		requestSdrEntryFit();
		noteSdrRefState({ ref: -55, ref_set: true });
		maybeRequestSdrFit();
		expect(sent).toEqual([]);
		expect(getDisplayRef()).toBe(-55 + offset);
		expect(autoScaleBusy()).toBe(false);
	});

	it.each(['entry', 'explicit'])('fits %s Auto in raw units with a +60 offset and keeps marker placement stable', kind => {
		graphMode.confirm('sdr');
		commitLevelOffset(60);
		requestSdrEntryFit();
		noteSdrRefState({ ref: -55, ref_set: kind === 'explicit' });
		expect(getDisplayRef()).toBe(5);
		if (kind === 'entry') maybeRequestSdrFit();
		else autoScaleRequest();
		expect(sent.at(-1)).toMatchObject({ cmd: 'AUTO_SCALE', current_ref: -55, range_db: 100 });
		expect(sent.at(-1)?.display_only).toBe(kind === 'entry' ? true : undefined);
		acknowledge(1);
		const result = { result: 'applied', target: -30, last_peak: -65, last_noise_floor: -115, seq: 2 };
		status(result);
		expect(getDisplayRef()).toBe(30);
		const rect = plotRect();
		const markerY = getY(-65);
		expect(markerY).toBeCloseTo(rect.y + rect.h * 0.35, 6);
		expect(getY(-115)).toBeCloseTo(rect.y + rect.h * 0.85, 6);
		status(result);
		expect(getDisplayRef()).toBe(30);
		expect(getY(-65)).toBeCloseTo(markerY, 6);
		commitLevelOffset(0);
		status(result);
		expect(getDisplayRef()).toBe(-30);
		expect(getY(-65)).toBeCloseTo(markerY, 6);
		expect(autoScaleBusy()).toBe(false);
		expect(sent).toHaveLength(1);
	});

	it('supersedes pending manual Ref without its old timeout rolling back Auto', () => {
		graphMode.confirm('sdr');
		setDisplayRef('user', -30);
		refLevel.set(-30);
		autoScaleRequest();
		expect(sent.at(-1)?.current_ref).toBe(-30);
		expect(pendingDisplayRef()).toBeNull();
		expect(refLevel.pending()).toBe(false);
		acknowledge(1);
		status({ result: 'applied', target: -85, seq: 2 });
		vi.advanceTimersByTime(6000);
		expect(getDisplayRef()).toBe(-85);
	});

	it.each([-200, 100])('fits an out-of-hardware viewport at %s using valid wire bounds', ref => {
		graphMode.confirm('sdr');
		setDisplayRef('user', ref);
		autoScaleRequest();
		expect(sent.at(-1)?.current_ref).toBe(ref < -160 ? -160 : 40);
		acknowledge(1);
		status({ result: 'applied', target: -30, seq: 2 });
		expect(getDisplayRef()).toBe(-30);
	});

	it('prepares explicit SDR recovery audio once for known or newly reported overflow', () => {
		graphMode.confirm('sdr');
		syncAutoScaleStatus({ status_warning: -12 });
		autoScaleRequest();
		acknowledge(1, true);
		status({ result: 'adjusting', adjusting: true, hardware_adjusting: true, seq: 1 });
		expect(prepareSdrAudioTransition).toHaveBeenCalledTimes(1);
		status({ result: 'applied', target: -20, seq: 2 });
		syncAutoScaleStatus({ status_warning: 0 });
		autoScaleRequest();
		acknowledge(3);
		expect(prepareSdrAudioTransition).toHaveBeenCalledTimes(1);
		status({ result: 'adjusting', adjusting: true, hardware_adjusting: true, seq: 3 });
		expect(prepareSdrAudioTransition).toHaveBeenCalledTimes(2);
	});
});

describe('Auto session progress and cancellation', () => {
	it('stays busy beyond four seconds after backend acknowledgement, until the final sequence', () => {
		autoScaleRequest();
		acknowledge(1);
		vi.advanceTimersByTime(12000);
		expect(autoScaleBusy()).toBe(true);
		expect(document.getElementById('btn-ref-auto')?.classList.contains('busy')).toBe(true);
		status({ result: 'applied', target: -10, seq: 2, adjusting: false });
		expect(autoScaleBusy()).toBe(false);
		expect(document.getElementById('btn-ref-auto')?.classList.contains('busy')).toBe(false);
		expect(S.noticeText).toBe('Ref → -10 dBm');
	});

	it('lets unanswered requests expire locally', () => {
		autoScaleRequest();
		vi.advanceTimersByTime(4100);
		expect(autoScaleBusy()).toBe(false);
	});

	it('ignores an old session ACK and final while waiting for the latest request ACK', () => {
		graphMode.confirm('sdr');
		autoScaleRequest();
		const oldId = sent.at(-1)?.request_id;
		clearAutoScaleHint();
		setDisplayRef('user', -40);
		autoScaleRequest();
		syncAutoScaleStatus({ response_to: 'AUTO_SCALE', request_id: oldId,
			auto_ref: { result: 'adjusting', adjusting: true, seq: 1 } });
		status({ result: 'applied', target: -15, seq: 2 });
		expect(getDisplayRef()).toBe(-40);
		expect(autoScaleBusy()).toBe(true);
		acknowledge(4);
		status({ result: 'applied', target: -70, seq: 5 });
		expect(getDisplayRef()).toBe(-70);
	});

	it.each([-200, -160, 40, 100])('keeps Preset fit armed through idle and retired sessions at Ref %s', ref => {
		graphMode.confirm('sdr');
		autoScaleRequest();
		const retiredId = sent.at(-1)?.request_id;
		acknowledge(10);
		clearAutoScaleHint();
		setDisplayRef('user', ref);
		refLevel.set(Math.max(-160, Math.min(40, ref)));
		resetAutoScaleState();
		requestSdrEntryFit(false);
		noteSdrRefState({ ref: 0, ref_set: false });
		status({ result: 'idle', target: null, adjusting: false, seq: 12 });
		status({ result: 'applied', target: -20, adjusting: false, seq: 11 });
		expect(getDisplayRef()).toBe(ref);
		expect(pendingDisplayRef()).toBeNull();
		expect(refLevel.pending()).toBe(false);
		maybeRequestSdrFit();
		expect(sent.at(-1)).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true, current_ref: Math.max(-160, Math.min(40, ref)) });
		expect(sent).toHaveLength(2);
		syncAutoScaleStatus({ response_to: 'AUTO_SCALE', request_id: retiredId,
			auto_ref: { result: 'adjusting', adjusting: true, seq: 10 } });
		status({ result: 'applied', target: -20, adjusting: false, seq: 11 });
		expect(getDisplayRef()).toBe(ref);
		acknowledge(13);
		status({ result: 'applied', target: -20, adjusting: false, seq: 11 });
		expect(getDisplayRef()).toBe(ref);
		status({ result: 'applied', target: -30, adjusting: false, seq: 14 });
		expect(getDisplayRef()).toBe(-30);
		vi.advanceTimersByTime(6000);
		maybeRequestSdrFit();
		expect(getDisplayRef()).toBe(-30);
		expect(autoScaleBusy()).toBe(false);
		expect(sent).toHaveLength(2);
	});

	it.each(['SET_REF', 'SET_AMP', 'SET_FREQ', 'SET_TRIGGER', 'SET_MODE', 'SET_SDR']) (
		'cancels immediately on %s and ignores every queued old target', cmd => {
			graphMode.confirm('sdr');
			autoScaleRequest();
			acknowledge(1);
			send({ cmd, ref: -40 });
			setDisplayRef('user', -40);
			status({ result: 'adjusting', adjusting: true, seq: 1 });
			status({ result: 'applied', target: -15, seq: 2 });
			status({ result: 'cancelled', target: null, seq: 3 });
			expect(getDisplayRef()).toBe(-40);
			expect(autoScaleBusy()).toBe(false);
			expect(S.noticeText).toBeNull();
		},
	);

	it('ignores sticky and out-of-order final results after a later placement', () => {
		graphMode.confirm('sdr');
		autoScaleRequest();
		acknowledge(5);
		status({ result: 'applied', target: -70, seq: 6 });
		setDisplayRef('user', -40);
		noteDisplayRefReport(-40);
		status({ result: 'applied', target: -70, seq: 6 });
		status({ result: 'applied', target: -20, seq: 4 });
		expect(getDisplayRef()).toBe(-40);
	});
});
describe('Auto terminal outcomes', () => {
	it.each([
		['ok', 'Reference already fits'],
		['no_data', 'Waiting for a trace'],
		['limit', 'Auto stopped at the reference limit'],
		['stalled', 'Auto stopped: the device reference did not advance'],
		['failed', 'Auto could not adjust the reference'],
		['cancelled', 'Auto cancelled'],
		['overflow', 'Display fit stopped: raise the hardware reference or press Auto'],
		['timeout', 'Auto timed out before the reference was verified'],
	])('ends busy on %s without applying or announcing a stale target', (result, notice) => {
		graphMode.confirm('sdr');
		setDisplayRef('preset', -40);
		autoScaleRequest();
		acknowledge(1);
		status({ result, target: -10, adjusting: false, seq: 2 });
		expect(getDisplayRef()).toBe(-40);
		expect(autoScaleBusy()).toBe(false);
		expect(S.noticeText).toBe(notice);
	});

	it('forgets a previous target on no_data so the same later placement can apply again', () => {
		graphMode.confirm('sdr');
		status({ result: 'applied', target: -70, seq: 1 });
		status({ result: 'no_data', target: null, seq: 2 });
		setDisplayRef('preset', -20);
		status({ result: 'applied', target: -70, seq: 3 });
		expect(getDisplayRef()).toBe(-70);
	});

	it('retries a display-only entry after no_data only after the retry interval', () => {
		graphMode.confirm('sdr');
		requestSdrEntryFit(false);
		noteSdrRefState({ ref: 0, ref_set: false });
		maybeRequestSdrFit();
		acknowledge(1);
		status({ result: 'no_data', target: null, seq: 2 });
		maybeRequestSdrFit();
		expect(sent).toHaveLength(1);
		vi.advanceTimersByTime(1600);
		maybeRequestSdrFit();
		expect(sent).toHaveLength(2);
		expect(sent[1].display_only).toBe(true);
		acknowledge(3);
		status({ result: 'applied', target: -95, seq: 4 });
		vi.advanceTimersByTime(1600);
		maybeRequestSdrFit();
		expect(sent).toHaveLength(2);
		expect(getDisplayRef()).toBe(-95);
	});

	it('finishes a Preset fit on overflow without moving Ref or retrying hardware recovery', () => {
		graphMode.confirm('sdr');
		setDisplayRef('user', -160);
		resetAutoScaleState();
		requestSdrEntryFit(false);
		noteSdrRefState({ ref: 0, ref_set: false });
		status({ result: 'idle', target: null, adjusting: false, seq: 1 });
		maybeRequestSdrFit();
		acknowledge(2);
		syncAutoScaleStatus({ status_warning: -12,
			auto_ref: { result: 'overflow', target: null, adjusting: false, seq: 3 } });
		expect(getDisplayRef()).toBe(-160);
		expect(S.noticeText).toBe('Display fit stopped: raise the hardware reference or press Auto');
		expect(autoScaleBusy()).toBe(false);
		vi.advanceTimersByTime(1600);
		maybeRequestSdrFit();
		expect(sent).toHaveLength(1);
		expect(sent[0]).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true });
		expect(prepareSdrAudioTransition).not.toHaveBeenCalled();
	});

	it('withdraws a no-data notice once a trace is available', () => {
		status({ result: 'no_data', target: null, seq: 1 });
		expect(S.noticeText).toBe('Waiting for a trace');
		noteTraceObservation({ last_peak: -80, last_noise_floor: -110 });
		expect(S.noticeText).toBeNull();
	});
});
