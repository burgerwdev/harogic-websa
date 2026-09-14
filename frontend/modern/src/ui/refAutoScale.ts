/**
 * Auto Scale - one-shot reference placement (the Ref panel's Auto button).
 *
 * The reference used to be a tracking mode: the backend re-decided every frame and paid a full
 * device reconfiguration whenever the value changed. Measured on the bench, one correction takes
 * 1.86 s while the loop sat idle afterwards, and nothing in the UI showed that anything was
 * happening. So "Auto" is an action, not a mode (as on a bench analyser: Keysight Auto Scale,
 * R&S Auto Level, Anritsu Auto Scale), and pressing it must show what it is doing.
 *
 * One implementation, three modes: the backend runs the placement rule (`AUTO_SCALE`) - SDR
 * included, where the target is still its IQS level. SDR's *display* scale is client-side, so this
 * module applies the reported target to the display reference there; in SWP/RTA the displayed
 * scale is the device reference, which the backend has already applied.
 */
import { t } from '../core/i18n';
import { getDisplayRef, setDisplayRef } from './displayRef';
import { send } from '../core/wsSend';
import * as S from '../core/store';
import { currentGraphMode } from './graphMode';
import { requestRender } from '../render/redraw';

/** Results that mean "a level was placed": the only ones the display may follow. */
const PLACED_RESULTS = new Set(['applied', 'clipped', 'below_window', 'overflow']);

/** How long the button keeps glowing when the backend gives no answer at all. */
const BUSY_FALLBACK_MS = 4000;
/** SDR: the display fit is asked for on the first frame after entering the mode. */
const ENTRY_FIT_MAX_ATTEMPTS = 12;
/** ...and repeated at most this often while the backend has no trace yet. */
const ENTRY_FIT_RETRY_MS = 500;

let busyUntil = 0;
let entryFitPending = false;
let entryFitAttempts = 0;
let nextEntryAttempt = 0;
/** The last decision the UI has seen (`auto_ref.seq`), so a sticky result is not mistaken for
 * the answer to a press that is still in flight. */
let lastSeq = -1;
/** SDR: the target that was already written to the display reference. */
let appliedTarget: number | null = null;

function button(): HTMLButtonElement | null {
	return document.getElementById('btn-ref-auto') as HTMLButtonElement | null;
}

/** Generation of the last notice, so an older timer cannot clear a newer message.
 *
 * Comparing the text instead looked fine until the same message was shown twice in a row (an
 * entry fit and a press both reporting "no signal"): the first timer then wiped the second one
 * early, and the user saw nothing. */
let noticeSeq = 0;

/**
 * What the current notice is about.
 *
 * A *refusal* ("no signal to fit", "waiting for a trace") is a statement about the measurement in
 * front of the user, so it must be withdrawn as soon as that statement stops being true - it used
 * to sit there for its full 6 s while the signal was already on the display (reported). Timing
 * alone cannot know that, so the observation the decision was made from is kept here.
 */
let noticeAbout: { result: string; peak: number | null; floor: number | null } | null = null;

/** The observation has to move this much before a refusal is considered stale. */
const NOTICE_STALE_DB = 3.0;

/**
 * Post a transient message to the canvas status stack (under the warnings).
 *
 * It used to sit in the Ref parameter row, where every message moved the input, the buttons and
 * the arrows sideways; the group head only moved the problem. The canvas is also where the
 * condition this answers is drawn (`!IF overflow`), so cause and result are seen together.
 *
 * Canvas text is painted during a render pass, so both posting and clearing must ask for a
 * repaint: an overflowing IF sends no frames, and a message posted from that state would simply
 * never appear (DEVELOPMENT §8).
 */
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

/** Say what the fit decided. The level is always named when one was applied. */
function announce(result: string, target: unknown, a?: any): void {
	// The observation the decision came from, so a refusal can be withdrawn when it goes stale.
	const about = { result, peak: a?.last_peak ?? null, floor: a?.last_noise_floor ?? null };
	if (result === 'no_signal') postRefNotice(t('auto_scale_no_signal'), 6000, about);
	else if (result === 'no_data') postRefNotice(t('auto_scale_no_data'), 6000, about);
	else if (result === 'ok') postRefNotice(t('auto_scale_ok'), 2500, about);
	else if (target != null) {
		// 'applied', but also the automatic safety corrections ('clipped'/'below_window'/
		// 'overflow'): when the display moves on its own, the reason and the new level must be
		// visible.
		postRefNotice(`Ref \u2192 ${Math.round(Number(target))} dBm`, 3000, about);
	}
}

/** True while a fit is in flight: drives the glow (cleared by the STATUS handler). */
export function autoScaleBusy(): boolean {
	return performance.now() < busyUntil;
}

/** The user pressed Auto Scale. */
export function autoScaleRequest(): void {
	// A press supersedes an entry fit that is still looking for a trace.
	entryFitPending = false;
	appliedTarget = null;
	// range_db = the visible window height. The fit anchors the noise floor just above the bottom
	// of that window, and the backend cannot see the client's dB/div setting.
	send({
		cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv,
		// The level the user sees: in SDR the display scale is client-side, and judging the fit
		// against the device level instead left a clipped display unchanged (measured).
		current_ref: getDisplayRef(),
	});
	busyUntil = performance.now() + BUSY_FALLBACK_MS;
	setBusy(true);
}

/** Entering SDR asks for one fit as soon as a frame has been observed. */
export function requestSdrEntryFit(): void {
	entryFitPending = true;
	entryFitAttempts = 0;
	nextEntryAttempt = 0;
	appliedTarget = null;
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
	const now = performance.now();
	if (now < nextEntryAttempt) return;          // one request per settle period, not per frame
	if (entryFitAttempts++ >= ENTRY_FIT_MAX_ATTEMPTS) {
		entryFitPending = false;
		return;
	}
	nextEntryAttempt = now + ENTRY_FIT_RETRY_MS;
	send({
		cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv,
		current_ref: getDisplayRef(),
	});
	busyUntil = now + BUSY_FALLBACK_MS;
}

/**
 * STATUS arrived: clear the glow, follow the decision in SDR and report the outcome.
 *
 * `auto_ref.result` is sticky, so only a change is announced - otherwise the message would
 * reappear after every unrelated status.
 */
export function syncAutoScaleStatus(s: any): void {
	const a = s?.auto_ref;
	if (!a) return;
	const result = String(a.result ?? 'idle');
	const target = a.target;
	const seq = Number(a.seq ?? -1);

	// While the device is still settling, a decision is not yet the final word: keep glowing and
	// do not consume the sequence number, so the settled report is still treated as the answer.
	if (a.adjusting) {
		setBusy(autoScaleBusy());
		return;
	}
	if (result === 'idle' || seq === lastSeq) {
		// No new decision: a press that is still in flight keeps its glow.
		setBusy(autoScaleBusy());
		return;
	}
	lastSeq = seq;

	if (currentGraphMode() === 'sdr' && result !== 'no_data') {
		// The backend answered, so the entry fit has done its job: without this the request was
		// repeated on every frame, and each of those decisions cleared the button's glow.
		entryFitPending = false;
		const before = Math.round(getDisplayRef());
		// The display scale belongs to the client: apply the level the backend placed. Only the
		// results that come back from an application do that - a refusal carries a stale target
		// (`auto_ref.target` is the last APPLIED level) and must never move the display - and the
		// target check skips a repeat.
		//
		// The names mirror auto_reference.py's vocabulary: 'applied' from a fit, 'clipped' and
		// 'below_window' when the fit itself classified the placement, 'overflow' from the IF
		// escape. Renaming one there without updating this list is what the e2e caught once (the
		// SDR correction stopped reaching the screen).
		//
		// An automatic correction moves the display even when the user set the level by hand: it
		// fires precisely because that level left the trace clipped or off-screen, and the hint
		// names the new level, so leaving the canvas alone announced a change that never happened
		// (reported: "Ref decreased to -50 dBm, warning, hint says it adjusted, but the trace and
		// the Ref box did not move"). A queued update from before the manual edit is dropped by the
		// backend epoch instead.
		const placed = PLACED_RESULTS.has(result);
		const move = placed && target != null && Number(target) !== appliedTarget;
		if (move) {
			appliedTarget = Number(target);
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
}

/** Preset (or a fresh start): forget what Auto last did, so nothing stale blocks a new fit. */
export function resetAutoScaleState(): void {
	lastSeq = -1;
	appliedTarget = null;
	entryFitPending = false;
	busyUntil = 0;
	setBusy(false);
}

/**
 * A manual Ref edit happened: forget the pending bookkeeping and any leftover message.
 *
 * A sticky STATUS is already harmless - it carries the sequence number of a decision the UI has
 * seen, so it is never mistaken for a new answer (`syncAutoScaleStatus`) - and clearing
 * `appliedTarget` here means a later press may land on the same level again.
 */
export function clearAutoScaleHint(): void {
	entryFitPending = false;
	busyUntil = 0;
	appliedTarget = null;                  // a new press may land on the same level again
	setBusy(false);
	// Drop a notice left over from an earlier decision: it describes a level that is no longer the
	// one being asked for, and reads as "the app adjusted something just now".
	withdrawNotice();
}
