/**
 * Auto Scale - one-shot reference placement (the Ref panel's Auto button).
 *
 * Auto Scale is an explicit one-shot action with visible progress.
 *
 * The backend runs a bounded recovery and placement session for explicit Auto requests.
 * SDR entry instead requests display_only placement. SDR's display scale is client-owned;
 * only a successful final placement may move it.
 */
import { t } from '../core/i18n';
import { cancelDisplayRefRequest, displayRefDiverges, getDisplayRef, setDisplayRef } from './displayRef';
import { onCommandSent, send } from '../core/wsSend';
import * as S from '../core/store';
import { currentGraphMode } from './graphMode';
import { requestRender } from '../render/redraw';
import { prepareSdrAudioTransition } from '../audio/sdrAudio';
import { refLevel } from './refState';

interface AutoRefStatus {
	result?: unknown;
	target?: unknown;
	seq?: unknown;
	adjusting?: boolean;
	hardware_adjusting?: boolean;
	last_peak?: number | null;
	last_noise_floor?: number | null;
}

/** Only successful placement is allowed to change the SDR display reference. */
const PLACED_RESULTS: Record<string, true> = { applied: true };

/** How long the button keeps glowing when the backend gives no answer at all. */
const BUSY_FALLBACK_MS = 4000;
/** SDR: the display fit is asked for on the first frame after entering the mode. */
const ENTRY_FIT_MAX_ATTEMPTS = 12;
/** Minimum retry interval while the backend has no trace to fit. */
const ENTRY_FIT_RETRY_MS = 1600;

let busyUntil = 0;
let backendAdjusting = false;
let ignoreAutoStatus = false;
let awaitingAutoAck = false;
let autoRequestSerial = 0;
let pendingAutoRequestId: string | null = null;
let highestSeq = -1;
let explicitSdrAuto = false;
let audioTransitionPrepared = false;
let lastStatusWarning = 0;
let entryFitPending = false;
let entryFitAttempts = 0;
/** At most one entry-fit request may be outstanding; retry no_data only after its answer. */
let entryFitInFlight = false;
let nextEntryAttempt = 0;
/** The last decision the UI has seen (`auto_ref.seq`), so a sticky result is not mistaken for
 * the answer to a press that is still in flight. */
let lastSeq = -1;
/** SDR: the target that was already written to the display reference. */
let appliedTarget: number | null = null;

function button(): HTMLButtonElement | null {
	return document.getElementById('btn-ref-auto') as HTMLButtonElement | null;
}

/** Notice generation prevents older timers from clearing newer notices. */
let noticeSeq = 0;

/** Source observation used to withdraw refusal notices once they become stale. */
let noticeAbout: { result: string; peak: number | null; floor: number | null } | null = null;

/** The observation has to move this much before a refusal is considered stale. */
const NOTICE_STALE_DB = 3.0;

/** Post a canvas status notice and repaint independently of incoming frames. */
export function postRefNotice(text: string, holdMs = 4000,
                              about: typeof noticeAbout = null): void {
	const id = ++noticeSeq;
	noticeAbout = about;
	S.setNoticeText(text);
	requestRender();
	if (holdMs > 0) window.setTimeout(() => {
		if (id !== noticeSeq) return;
		noticeAbout = null;
		S.setNoticeText(null);
		requestRender();
	}, holdMs);
}

function withdrawNotice(): void {
	noticeSeq++;                       // cancel the pending timer and any older clear
	noticeAbout = null;
	S.setNoticeText(null);
	requestRender();
}

/**
 * A trace observation arrived (STATUS carries the newest peak/floor the backend saw).
 *
 * Withdraws a refusal that no longer describes reality: "waiting for a trace" stops being true
 * with the first trace, and "no signal to fit" stops being true when the peak moves away from the
 * level the decision was based on.
 */
export function noteTraceObservation(a: any): void {
	if (!a || noticeAbout === null || S.noticeText === null) return;
	const peak = a.last_peak == null ? null : Number(a.last_peak);
	if (peak === null || !isFinite(peak)) return;
	if (noticeAbout.result === 'no_data') {
		withdrawNotice();
		return;
	}
	if (noticeAbout.result === 'no_signal') {
		const was = noticeAbout.peak;
		if (was === null || Math.abs(peak - was) >= NOTICE_STALE_DB) withdrawNotice();
	}
}

function setBusy(on: boolean): void {
	const b = button();
	if (b) b.classList.toggle('busy', on);
}

/** Say what the session decided; failures never announce an old target as a placement. */
function announce(result: string, target: unknown, a?: AutoRefStatus): void {
	const about = { result, peak: a?.last_peak ?? null, floor: a?.last_noise_floor ?? null };
	if (result === 'no_data') postRefNotice(t('auto_scale_no_data'), 6000, about);
	else if (result === 'ok') postRefNotice(t('auto_scale_ok'), 2500, about);
	else if (result === 'applied' && target != null && Number.isFinite(Number(target))) {
		postRefNotice(`Ref \u2192 ${Math.round(Number(target))} dBm`, 3000, about);
	} else if (['limit', 'stalled', 'failed', 'cancelled', 'overflow', 'timeout'].includes(result)) {
		postRefNotice(t(`auto_scale_${result}`), 6000, about);
	}
}

/** True while a fit is in flight: drives the glow (cleared by the STATUS handler). */
export function autoScaleBusy(): boolean {
	return backendAdjusting || performance.now() < busyUntil;
}

/** Whether SDR already has a user-owned hardware level; fitting affects the client display. */
let sdrHasOwnLevel = false;

/** One-shot: on entry the display scale follows the level the SDR itself stores. */
let entrySyncPending = false;
/** The SDR level the last STATUS reported. */
let reportedSdrRef: number | null = null;

/** Remember the reported SDR reference and synchronize the display once per entry. */
export function noteSdrRefState(sdr: any): void {
	const ref = Number(sdr?.ref);
	if (isFinite(ref)) reportedSdrRef = ref;
	sdrHasOwnLevel = sdr?.ref_set === true;
	// Diagnostic/e2e hook (same pattern as `dataset.notice`): what this hand-off decided, and why.
	const note = (what: string) => {
		const cv = document.getElementById('spectrum');
		const value = JSON.stringify({ what, ref: reportedSdrRef, refSet: sdrHasOwnLevel,
		                                display: getDisplayRef() });
		if (cv && cv.dataset.sdrEntry !== value) cv.dataset.sdrEntry = value;
	};
	if (currentGraphMode() !== 'sdr' || reportedSdrRef === null) { note('not-sdr'); return; }
	if (!entrySyncPending) { note(sdrHasOwnLevel ? 'own-level' : 'fit-armed'); return; }
	entrySyncPending = false;
	// Synchronize once after a finite SDR reference arrives, unless user intent owns the display.
	if (displayRefDiverges()) { note('user-request-in-flight'); return; }   // it owns the display
	if (Math.abs(reportedSdrRef - getDisplayRef()) < 0.5) { note('already-on-it'); return; }
	setDisplayRef('sdr', reportedSdrRef);
	note('synced');
}

/** Leaving SDR (or a preset): the entry hand-off starts over on the next visit. */
export function clearSdrEntryState(): void {
	entrySyncPending = false;
	reportedSdrRef = null;
}

/** The user pressed Auto Scale. */
export function autoScaleRequest(): void {
	cancelDisplayRefRequest();
	refLevel.reset();
	document.getElementById('input-ref')?.classList.remove('ref-pending');
	ignoreAutoStatus = false;
	awaitingAutoAck = true;
	pendingAutoRequestId = `ref-auto-${++autoRequestSerial}`;
	backendAdjusting = false;
	explicitSdrAuto = currentGraphMode() === 'sdr';
	audioTransitionPrepared = false;
	if (explicitSdrAuto && lastStatusWarning === -12) prepareAutoAudioTransition();
	// A press supersedes an entry fit that is still looking for a trace.
	entryFitPending = false;
	entryFitInFlight = false;
	appliedTarget = null;
	// range_db = the visible window height. The fit anchors the noise floor just above the bottom
	// of that window, and the backend cannot see the client's dB/div setting.
	send({
		cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv,
		request_id: pendingAutoRequestId,
		// Fit against the visible client display level.
		current_ref: getDisplayRef(),
	});
	busyUntil = performance.now() + BUSY_FALLBACK_MS;
	setBusy(true);
}

/**
 * Arm the entry hand-off for SDR (see `noteSdrRefState`) and one fit for a level-less SDR.
 *
 * For Preset, pass false to retain the display scale until the fit's absolute target arrives.
 */
export function requestSdrEntryFit(syncToReportedLevel = true): void {
	ignoreAutoStatus = false;
	backendAdjusting = false;
	explicitSdrAuto = false;
	awaitingAutoAck = false;
	entryFitPending = true;
	entrySyncPending = syncToReportedLevel;
	entryFitAttempts = 0;
	nextEntryAttempt = 0;
	appliedTarget = null;
	entryFitInFlight = false;
	busyUntil = performance.now() + 3000;
	setBusy(true);
}

/**
 * Called for every SDR frame: place the reference once after entering the mode.
 *
 * The backend needs a trace to fit, so the request is repeated until it has one (or the attempt
 * budget runs out, in which case the user can press Auto).
 */
export function maybeRequestSdrFit(): void {
	if (!entryFitPending || currentGraphMode() !== 'sdr') return;
	// A level-less SDR (a preset, or the first SDR visit of this process) gets ONE placement, which
	// the client applies to its display scale - instant. An SDR that already has a level is left
	// alone: `noteSdrRefState` has just brought the display back onto that level, and a fit would
	// only move a scale the operator chose while looking at the trace.
	if (sdrHasOwnLevel) {
		entryFitPending = false;
		busyUntil = 0;
		setBusy(false);
		return;
	}
	const now = performance.now();
	if (now < nextEntryAttempt || entryFitInFlight) return;   // one request outstanding at a time
	if (entryFitAttempts++ >= ENTRY_FIT_MAX_ATTEMPTS) {
		entryFitPending = false;
		busyUntil = 0;
		setBusy(false);
		return;
	}
	nextEntryAttempt = now + ENTRY_FIT_RETRY_MS;
	entryFitInFlight = true;
	awaitingAutoAck = true;
	pendingAutoRequestId = `ref-auto-${++autoRequestSerial}`;
	send({
		cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv,
		current_ref: getDisplayRef(),
		display_only: true,
		request_id: pendingAutoRequestId,
	});
	busyUntil = now + BUSY_FALLBACK_MS;
}

/**
 * STATUS arrived: keep the glow for the whole backend session, then follow its final placement.
 * The backend acknowledges each request with adjusting before producing a terminal result.
 */
export function syncAutoScaleStatus(s: { status_warning?: unknown; response_to?: unknown; request_id?: unknown; auto_ref?: AutoRefStatus }): void {
	if (s?.status_warning != null) lastStatusWarning = Number(s.status_warning);
	const a = s?.auto_ref;
	if (!a) return;
	const result = String(a.result ?? 'idle');
	const target = a.target;
	const seq = Number(a.seq ?? -1);
	if (!Number.isFinite(seq) || seq < highestSeq) return;
	if (ignoreAutoStatus) {
		highestSeq = Math.max(highestSeq, seq);
		return;
	}
	const acknowledged = pendingAutoRequestId !== null && s.response_to === 'AUTO_SCALE'
		&& s.request_id === pendingAutoRequestId;
	if (awaitingAutoAck && !acknowledged) return;
	if (acknowledged) {
		awaitingAutoAck = false;
		pendingAutoRequestId = null;
	}
	if (a.adjusting) {
		highestSeq = seq;
		awaitingAutoAck = false;
		backendAdjusting = true;
		if (explicitSdrAuto && a.hardware_adjusting) prepareAutoAudioTransition();
		setBusy(true);
		return;
	}
	if (awaitingAutoAck || seq === lastSeq) {
		setBusy(autoScaleBusy());
		return;
	}
	highestSeq = seq;
	lastSeq = seq;
	backendAdjusting = false;
	entryFitInFlight = false;
	if (!PLACED_RESULTS[result]) appliedTarget = null;

	if (currentGraphMode() === 'sdr' && result !== 'no_data') {
		// Any answer other than no_data completes the entry fit.
		entryFitPending = false;
		const before = Math.round(getDisplayRef());
		// Refusals and cancellation never move the scale, even if a stale target is present.
		const placed = PLACED_RESULTS[result] === true;
		const move = placed && target != null && Number.isFinite(Number(target)) && Number(target) !== appliedTarget;
		if (move) {
			appliedTarget = Number(target);
			// Apply the absolute fit target to the client display, preserving the IQS reference.
			setDisplayRef('auto', appliedTarget);
		}
		const cv = document.getElementById('spectrum');
		if (cv) {
			// `before`/`shown` make "the decision is what the user sees" checkable from outside;
			// the record is written for every decision, so "nothing needed changing" is visible
			// too (`applied: false`).
			const shown = Math.round(getDisplayRef());
			if (move) cv.dataset.sdrRef = String(shown);
			cv.dataset.sdrRefDbg = JSON.stringify({
				seq, result,
				noise: a.last_noise_floor == null ? null : Math.round(a.last_noise_floor),
				peak: a.last_peak == null ? null : Math.round(a.last_peak),
				range: S.totalDivs * S.dbPerDiv,
				ref: move ? shown : before, applied: move, before, shown,
			});
		}
		if (move) requestRender();
	}

	busyUntil = 0;                         // the device answered: drop the local fallback
	setBusy(false);
	announce(result, target, a);
	explicitSdrAuto = false;
}

/** Preset (or a fresh start): forget what Auto last did, so nothing stale blocks a new fit. */
export function resetAutoScaleState(): void {
	lastSeq = -1;
	highestSeq = -1;
	backendAdjusting = false;
	ignoreAutoStatus = false;
	awaitingAutoAck = false;
	pendingAutoRequestId = null;
	explicitSdrAuto = false;
	audioTransitionPrepared = false;
	lastStatusWarning = 0;
	appliedTarget = null;
	entryFitPending = false;
	entryFitInFlight = false;
	busyUntil = 0;
	setBusy(false);
}

/**
 * Manual intent cancels the pending fit immediately. Ignore its queued ACK/final target until
 * another local Auto request (or SDR entry fit) establishes a new session.
 */
export function clearAutoScaleHint(): void {
	ignoreAutoStatus = true;
	backendAdjusting = false;
	awaitingAutoAck = false;
	pendingAutoRequestId = null;
	explicitSdrAuto = false;
	entrySyncPending = false;
	entryFitPending = false;
	entryFitInFlight = false;
	busyUntil = 0;
	appliedTarget = null;                  // a new press may land on the same level again
	setBusy(false);
	// Drop a notice left over from an earlier decision: it describes a level that is no longer the
	// one being asked for, and reads as "the app adjusted something just now".
	withdrawNotice();
}

function prepareAutoAudioTransition(): void {
	if (audioTransitionPrepared) return;
	audioTransitionPrepared = true;
	prepareSdrAudioTransition();
}

onCommandSent(command => {
	const cmd = String(command.cmd ?? '');
	if (cmd.startsWith('SET_') && !(cmd === 'SET_REF' && command.mode === 'auto')) {
		clearAutoScaleHint();
	}
});
