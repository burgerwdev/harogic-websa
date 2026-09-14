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
/**
 * SDR: does Auto own the display scale?
 *
 * True after entering the mode or pressing Auto. A manual Ref edit takes the scale over, and from
 * then on only an explicit press may move it again - an autonomous safety correction updates the
 * device level, not the level the user just chose.
 */
let ownDisplay = true;

function button(): HTMLButtonElement | null {
	return document.getElementById('btn-ref-auto') as HTMLButtonElement | null;
}

/** Generation of the last hint, so an older timer cannot clear a newer message.
 *
 * Comparing the text instead looked fine until the same message was shown twice in a row (an
 * entry fit and a press both reporting "no signal"): the first timer then wiped the second one
 * early, and the user saw nothing. */
let hintSeq = 0;

function hint(text: string, holdMs = 4000): void {
	const el = document.getElementById('ref-hint');
	if (!el) return;
	const id = ++hintSeq;
	el.textContent = text;
	if (holdMs > 0) window.setTimeout(() => {
		if (id === hintSeq) el.textContent = '';
	}, holdMs);
}

function setBusy(on: boolean): void {
	const b = button();
	if (b) b.classList.toggle('busy', on);
}

/** Say what the fit decided. The level is always named when one was applied. */
function announce(result: string, target: unknown): void {
	if (result === 'no_signal') hint(t('auto_scale_no_signal'), 6000);
	else if (result === 'no_data') hint(t('auto_scale_no_data'), 6000);
	else if (result === 'ok') hint(t('auto_scale_ok'), 2500);
	else if (target != null) {
		// 'applied', but also the automatic safety corrections ('out_of_window', 'overflow'):
		// when the display moves on its own, the reason and the new level must be visible.
		hint(`Ref \u2192 ${Math.round(Number(target))} dBm`, 3000);
	}
}

/** True while a fit is in flight: drives the glow (cleared by the STATUS handler). */
export function autoScaleBusy(): boolean {
	return performance.now() < busyUntil;
}

/** The user pressed Auto Scale. */
export function autoScaleRequest(): void {
	// A press supersedes an entry fit that is still looking for a trace, and claims the display.
	entryFitPending = false;
	appliedTarget = null;
	ownDisplay = true;
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
	ownDisplay = true;
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
		// results that come back from an application do that ('applied' and the automatic safety
		// corrections) - a refusal carries a stale target and must never move the display. The
		// target check skips a repeat, and `ownDisplay` keeps an autonomous correction away from a
		// level the user just set.
		const placed = result === 'applied' || result === 'out_of_window' || result === 'overflow';
		const move = placed && target != null && Number(target) !== appliedTarget && ownDisplay;
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
	announce(result, target);
}

/** Preset (or a fresh start): forget what Auto last did, so nothing stale blocks a new fit. */
export function resetAutoScaleState(): void {
	lastSeq = -1;
	appliedTarget = null;
	entryFitPending = false;
	ownDisplay = true;
	busyUntil = 0;
	setBusy(false);
}

/**
 * A manual Ref edit takes the level over.
 *
 * `appliedTarget` deliberately survives: the backend reports a sticky result, so a manual level
 * must not be overwritten by the same old decision on the next STATUS. A new press clears it
 * (see `autoScaleRequest`/`requestSdrEntryFit`), so a fresh fit can still land on that value.
 */
export function clearAutoScaleHint(): void {
	entryFitPending = false;
	ownDisplay = false;                    // the user owns the scale now
	busyUntil = 0;
	setBusy(false);
}
