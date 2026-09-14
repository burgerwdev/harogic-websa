/**
 * Auto Scale - one-shot reference placement (the Ref panel's Auto button).
 *
 * The reference used to be a tracking mode: the backend re-decided every frame and paid a full
 * device reconfiguration whenever the value changed. Measured on the bench, one correction
 * takes 1.86 s end to end and is a single step; after it the loop sat idle. So "Auto" is an
 * action, not a mode (as on a bench analyser: Keysight Auto Scale, R&S Auto Level, Anritsu
 * Auto Scale), and pressing it must show that something is happening - the old code gave no
 * feedback at all for those 1.86 s.
 *
 * SWP/RTA: the backend owns the fit (`AUTO_SCALE`), because the target depends on the
 * reference semantics and the display window. SDR: the display scale is client-side, so the fit
 * happens here from the current frame - and entering SDR fits once automatically.
 */
import { percentileApprox } from '../dsp/stats';
import { t } from '../core/i18n';
import { getDisplayRef, setDisplayRef } from './displayRef';
import { refLevel } from './refState';
import { resetSdrAutoRef } from '../core/sdrAutoRef';
import { send } from '../core/wsSend';
import * as S from '../core/store';
import { currentGraphMode } from './graphMode';

/** How long the button keeps glowing when the backend gives no answer at all. */
const BUSY_FALLBACK_MS = 4000;

let busyUntil = 0;
/** Set when SDR is entered: the next plausible panadapter frame is fitted once. */
let entryFitPending = false;
let lastResult = 'idle';

function button(): HTMLButtonElement | null {
	return document.getElementById('btn-ref-auto') as HTMLButtonElement | null;
}

function hint(text: string, holdMs = 4000): void {
	const el = document.getElementById('ref-hint');
	if (!el) return;
	el.textContent = text;
	if (holdMs > 0) window.setTimeout(() => {
		if (el.textContent === text) el.textContent = '';
	}, holdMs);
}

function setBusy(on: boolean): void {
	const b = button();
	if (b) b.classList.toggle('busy', on);
}

/** Say what the fit decided (SWP/RTA learn it from STATUS, SDR knows it locally). */
function announce(result: string, target: unknown): void {
	if (result === lastResult) return;
	lastResult = result;
	if (result === 'no_signal') hint(t('auto_scale_no_signal'), 6000);
	else if (result === 'no_data') hint(t('auto_scale_no_data'), 6000);
	else if (result === 'ok') hint(t('auto_scale_ok'), 2500);
	else if (result === 'applied' && target != null) {
		hint(`Ref \u2192 ${Math.round(Number(target))} dBm`, 2500);
	}
}

/** True while a fit is in flight: drives the glow (the STATUS handler clears it). */
export function autoScaleBusy(): boolean {
	return performance.now() < busyUntil;
}

/** The user pressed Auto Scale. */
export function autoScaleRequest(): void {
	if (currentGraphMode() === 'sdr') {
		// Client-side and instant: fit the display on the next frame (the trace is the input).
		entryFitPending = true;
		resetSdrAutoRef();
		lastResult = 'idle';
		busyUntil = performance.now() + 800;
		setBusy(true);
		return;
	}
	// A new press is a new event: the same result as last time must be announced again.
	lastResult = 'idle';
	// range_db = the visible window height: the fit anchors the noise floor just above the
	// bottom of that window, and the backend cannot see the client's dB/div setting.
	send({ cmd: 'AUTO_SCALE', range_db: S.totalDivs * S.dbPerDiv });
	busyUntil = performance.now() + BUSY_FALLBACK_MS;
	setBusy(true);
}

/** Entering SDR always fits once (the previous reference may belong to a swept window). */
export function requestSdrEntryFit(): void {
	entryFitPending = true;
	resetSdrAutoRef();
	lastResult = 'idle';
	busyUntil = performance.now() + 3000;
	setBusy(true);
}

/**
 * One-shot SDR display fit, evaluated on the frame the user is looking at.
 *
 * The old continuous version smoothed the peak and the noise floor with EMAs and only acted
 * every 400 ms, because it had to keep a fading signal from moving the display. A one-shot fit
 * needs none of that: it is asked for once, so the current frame *is* the answer.
 */
export function maybeFitSdrFrame(spec: Float32Array | null): void {
	if (!entryFitPending || currentGraphMode() !== 'sdr') return;
	if (!spec || spec.length === 0) return;
	let peak = -Infinity;
	for (let i = 0; i < spec.length; i++) {
		const v = spec[i];
		if (v > peak && isFinite(v)) peak = v;
	}
	if (!isFinite(peak)) return;
	// Same 30th-percentile noise floor the backend uses for the swept fit. The old guard here
	// (< -119 dBm) belonged to the EMAs of the continuous version and blocked the fit outright
	// on a quiet bench; percentileApprox returns its -220 floor when a frame has no usable bin.
	const noise = percentileApprox(spec, 0.3);
	if (!isFinite(noise) || noise <= -210) return;
	entryFitPending = false;
	const range = S.totalDivs * S.dbPerDiv;
	// Noise floor ~8 dB above the bottom; never clip the peak (>= 10 dB of headroom).
	let ref = Math.max(noise + range - 8, peak + 10);
	ref = Math.min(40, Math.max(-160, Math.ceil(ref / 5) * 5));
	const before = Math.round(getDisplayRef());
	const applied = Math.abs(ref - before) >= 3;
	if (applied) setDisplayRef('auto', ref);
	const shown = Math.round(getDisplayRef());
	// The IQS chain needs a sane level too, but only when it is actually off: writing it
	// reconfigures the capture and interrupts the audio.
	const device = refLevel.get();
	if (applied && Math.abs(ref - device) >= 3) send({ cmd: 'SET_REF', mode: 'manual', ref });
	const cv = document.getElementById('spectrum');
	if (cv) {
		// `before` is the level the canvas had, `shown` the one it renders now: the pair is what
		// makes "the decision is what the user sees" checkable from the outside.
		cv.dataset.sdrRefDbg = JSON.stringify({
			noise: Math.round(noise), peak: Math.round(peak), range,
			ref: Math.round(ref), applied, before, shown,
		});
		if (applied) cv.dataset.sdrRef = String(ref);
	}
	// The fit is done (display ref written, device level sent when it was off): report it and
	// keep the confirmation glow just long enough to be visible.
	busyUntil = performance.now() + 600;
	setBusy(true);
	announce('applied', ref);
}

/**
 * STATUS arrived: clear the glow and report what the fit decided.
 *
 * `auto_ref.result` is the backend's outcome for the last one-shot fit ('ok' = already placed
 * well, 'no_signal' = nothing to anchor to, 'no_data' = no trace yet). It is sticky, so only a
 * change is announced - otherwise the message would reappear after every unrelated status.
 */
export function syncAutoScaleStatus(s: any): void {
	const a = s?.auto_ref;
	if (!a) return;
	if (currentGraphMode() === 'sdr') {
		// The SDR fit is client-side: its result was announced where it happened, so the
		// swept tracker's (possibly stale) result must not speak for it.
		setBusy(autoScaleBusy());
		return;
	}
	if (a.adjusting) {
		// A change is queued or still settling: keep glowing, but never extend the deadline -
		// `adjusting` is also true for settle windows this press did not ask for.
		setBusy(autoScaleBusy());
		return;
	}
	const result = String(a.result ?? 'idle');
	if (result === 'idle') {               // no decision to report yet
		setBusy(autoScaleBusy());
		return;
	}
	busyUntil = 0;                         // the device answered: drop the local fallback
	setBusy(false);
	announce(result, a.target);
}

/** A manual Ref edit takes the level over: there is nothing left to report. */
export function clearAutoScaleHint(): void {
	lastResult = 'idle';
	entryFitPending = false;
	busyUntil = 0;
	setBusy(false);
}
