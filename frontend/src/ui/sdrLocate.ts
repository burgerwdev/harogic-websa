/** Preview and click state only; the canvas owner routes gestures and submits tuning. */
import { ctx } from '../core/store';
import * as S from '../core/store';
import { plotRect } from '../render/plot';
import { requestRender } from '../render/redraw';
import { canvasColors } from '../core/theme';
import { t } from '../core/i18n';
import { formatFreqHz } from '../core/fmt';
import {
	carrierEvidenceVersion, currentCarrierEvidence, findCarrierCandidate, resetCarrierEvidence,
} from '../dsp/carrierSnap';
import {
	SNAP_ENVELOPE_MS, SNAP_WINDOW_HZ, SNAP_WINDOW_PX, SNAP_RELEASE_PX,
	alignToChannelGrid, signalLocateSupported,
} from './sdrTuning';
import type { LocateMode } from './sdrState';
import {
	effectiveLocateMode, sdrChannelAnchorHz, sdrChannelStepHz, sdrDemod, sdrIfbw,
	sdrListenHz, sdrLocateMode, sdrPitchHz,
} from './sdrState';
import { getCapture, getView, isZoomed } from './spectrumViewport';
import { postRefNotice } from './refAutoScale';

const AM_SNAP_CAP_HZ = 10_000;
const CW_SNAP_CAP_HZ = 3_000;
const AM_MAX_BIN_HZ = 1_000;
const CW_MAX_BIN_HZ = 250;

function signalMaxBinHz(mode: string): number {
	return mode === 'cw' ? Math.min(CW_MAX_BIN_HZ, sdrIfbw.get() / 2) : AM_MAX_BIN_HZ;
}

function locateView(): { lo: number; hi: number } | null {
	const view = isZoomed() ? getView() : getCapture();
	return view && view.hi > view.lo ? view : null;
}

export interface LocateTarget {
	mode: LocateMode;
	targetHz: number;
	carrierHz: number;
	x: number;
	evidenceMs: number | null;
}

function effectivePreference(mode: string): LocateMode {
	if (sdrLocateMode.get() === 'channel' && !(sdrChannelStepHz.get() >= 1)) return 'free';
	return effectiveLocateMode(mode);
}

export function snapRadiusHzAt(pixelScale: number): number {
	return signalRadiusHzAt(pixelScale, SNAP_WINDOW_PX);
}

function signalRadiusHzAt(pixelScale: number, pixels: number): number {
	const mode = sdrDemod.get(), view = locateView();
	if (!signalLocateSupported(mode) || !view) return 0;
	const cap = mode === 'cw' ? CW_SNAP_CAP_HZ : AM_SNAP_CAP_HZ;
	return Math.min(cap, pixels * Math.max(0, pixelScale) * (view.hi - view.lo) / Math.max(1, plotRect().w), SNAP_WINDOW_HZ);
}

/** Pure query: no hover mutation, persistence hold, or receiver commands. */
export function locateTargetAt(x: number, y: number, pixelScale: number): LocateTarget | null {
	const p = plotRect(), view = locateView();
	if (!view || x < p.x || x > p.x + p.w || y < p.y || y > p.y + p.h) return null;
	const raw = view.lo + (x - p.x) / p.w * (view.hi - view.lo);
	if (!Number.isFinite(raw) || raw <= 0) return null;
	const free: LocateTarget = { mode: 'free', targetHz: raw, carrierHz: raw, x, evidenceMs: null };
	if (!S.sdrMode) return free;
	if (raw < S.FREQ_MIN || raw > S.FREQ_MAX) return null;
	const mode = sdrDemod.get(), preference = effectivePreference(mode);
	if (preference === 'channel') {
		const aligned = alignToChannelGrid(raw, sdrChannelStepHz.get(), sdrChannelAnchorHz.get(),
			{ lo: Math.max(view.lo, S.FREQ_MIN), hi: Math.min(view.hi, S.FREQ_MAX) });
		if (aligned === null || aligned <= 0) return free;
		return { mode: 'channel', targetHz: aligned, carrierHz: aligned,
			x: p.x + (aligned - view.lo) / (view.hi - view.lo) * p.w, evidenceMs: null };
	}
	if (preference === 'signal') {
		const cand = findCarrierCandidate(raw, snapRadiusHzAt(pixelScale), performance.now(), signalMaxBinHz(mode));
		if (cand) {
			const dial = mode === 'cw' ? cand.hz - Math.max(100, sdrPitchHz.get()) : cand.hz;
			if (dial >= S.FREQ_MIN && dial <= S.FREQ_MAX && dial >= view.lo && dial <= view.hi) {
				return { mode: 'signal', targetHz: dial, carrierHz: cand.hz,
					x: p.x + (dial - view.lo) / (view.hi - view.lo) * p.w, evidenceMs: cand.atMs };
			}
		}
	}
	return free;
}

interface Environment {
	mode: string;
	listen: number;
	pitch: number;
	ifbw: number;
	locate: LocateMode;
	view: { lo: number; hi: number } | null;
	spacing: number;
	anchor: number;
	evidenceVersion: number;
}
interface Hover {
	x: number;
	y: number;
	pixelScale: number;
	target: LocateTarget | null;
	environment: Environment;
}
interface FrozenCandidate {
	target: LocateTarget | null;
	environment: Environment;
}
let hover: Hover | null = null;
let frozen: FrozenCandidate | null = null;
let staleTimer: number | null = null;
let staleDeadline: number | null = null;

function environmentSnapshot(): Environment {
	const view = locateView();
	return {
		mode: sdrDemod.get(), listen: sdrListenHz.get(), pitch: sdrPitchHz.get(), ifbw: sdrIfbw.get(),
		locate: effectivePreference(sdrDemod.get()), view: view ? { lo: view.lo, hi: view.hi } : null,
		spacing: sdrChannelStepHz.get(), anchor: sdrChannelAnchorHz.get(), evidenceVersion: carrierEvidenceVersion(),
	};
}

function environmentChanged(c: Environment): boolean {
	const view = locateView();
	return !S.sdrMode || c.mode !== sdrDemod.get() || c.listen !== sdrListenHz.get()
		|| c.pitch !== sdrPitchHz.get() || c.ifbw !== sdrIfbw.get()
		|| c.locate !== effectivePreference(sdrDemod.get()) || c.spacing !== sdrChannelStepHz.get()
		|| c.anchor !== sdrChannelAnchorHz.get() || c.evidenceVersion !== carrierEvidenceVersion()
		|| !view || !c.view || view.lo !== c.view.lo || view.hi !== c.view.hi;
}

function disarmStaleTimer(): void {
	if (staleTimer !== null) window.clearTimeout(staleTimer);
	staleTimer = null;
	staleDeadline = null;
}

/** Only a newer observation may extend the preview's deadline. */
function armStaleTimer(target: LocateTarget | null): void {
	const deadline = target?.mode === 'signal' ? (target.evidenceMs ?? -Infinity) + SNAP_ENVELOPE_MS : null;
	if (deadline === staleDeadline) return;
	disarmStaleTimer();
	if (deadline === null || typeof window === 'undefined') return;
	staleDeadline = deadline;
	staleTimer = window.setTimeout(() => {
		staleTimer = null;
		staleDeadline = null;
		requestRender();
	}, Math.max(0, deadline - performance.now()));
}

function refreshHover(): void {
	if (!hover) return;
	if (environmentChanged(hover.environment)) {
		hover.target = null;
		hover.environment = environmentSnapshot();
	}
	const h = hover, previous = h.target, now = performance.now();
	const view = locateView(), p = plotRect();
	if (!S.sdrMode || !view || h.x < p.x || h.x > p.x + p.w || h.y < p.y || h.y > p.y + p.h) {
		h.target = null;
		return;
	}
	const raw = view.lo + (h.x - p.x) / p.w * (view.hi - view.lo);
	if (previous?.mode === 'signal' && Math.abs(previous.carrierHz - raw) <= signalRadiusHzAt(h.pixelScale, SNAP_RELEASE_PX)) {
		const evidence = currentCarrierEvidence(previous.carrierHz, now, signalMaxBinHz(sdrDemod.get()));
		if (evidence) previous.evidenceMs = evidence.atMs;
		if (now - (previous.evidenceMs ?? -Infinity) < SNAP_ENVELOPE_MS) return;
	}
	const target = locateTargetAt(h.x, h.y, h.pixelScale);
	h.target = target?.mode === 'free' ? null : target;
	if (h.target?.mode === 'signal' && now - (h.target.evidenceMs ?? -Infinity) >= SNAP_ENVELOPE_MS) h.target = null;
}

/** Last displayed candidate, or the exact frozen candidate while pressed. */
export function sdrLocateHover(): LocateTarget | null {
	const state = frozen ?? hover;
	if (!state || environmentChanged(state.environment)) return null;
	const target = state.target;
	if (!target || target.mode === 'free') return null;
	if (target.mode === 'signal' && performance.now() - (target.evidenceMs ?? -Infinity) >= SNAP_ENVELOPE_MS) return null;
	return target;
}

export function updateSdrLocateHover(x: number, y: number, pixelScale: number): void {
	if (!S.sdrMode) { cancelSdrLocate(); return; }
	if (frozen) return;
	if (hover) {
		hover.x = x;
		hover.y = y;
		hover.pixelScale = pixelScale;
	} else hover = { x, y, pixelScale, target: null, environment: environmentSnapshot() };
	refreshHover();
	armStaleTimer(hover.target);
	requestRender();
}

/** Freeze the candidate already displayed at this pointer position. */
export function beginSdrLocateClick(x: number, y: number, pixelScale: number): boolean {
	if (!S.sdrMode) return false;
	const p = plotRect();
	if (x < p.x || x > p.x + p.w || y < p.y || y > p.y + p.h) return false;
	// Mousemove may round CSS pixels while pointerdown preserves fractions.
	const samePointer = hover && hover.pixelScale === pixelScale
		&& Math.abs(hover.x - x) <= pixelScale && Math.abs(hover.y - y) <= pixelScale;
	const displayed = samePointer && !environmentChanged(hover!.environment)
		? hover!.target : locateTargetAt(x, y, pixelScale);
	frozen = { target: displayed ? { ...displayed } : null, environment: environmentSnapshot() };
	requestRender();
	return true;
}

export function sdrLocateClickPending(): boolean { return frozen !== null; }

/** Release never substitutes a newly discovered carrier for the displayed bin. */
export function commitSdrLocateClick(rawHz: number): number | null {
	const c = frozen;
	frozen = null;
	requestRender();
	if (!c || !S.sdrMode || !Number.isFinite(rawHz) || rawHz < S.FREQ_MIN || rawHz > S.FREQ_MAX) return null;
	if (environmentChanged(c.environment)) {
		postRefNotice(t('sdr_locate_fallback_moved'), 2500);
		return rawHz;
	}
	const target = c.target;
	if (!target || target.mode === 'free') return rawHz;
	if (target.mode === 'signal') {
		const now = performance.now();
		const evidence = currentCarrierEvidence(target.carrierHz, now, signalMaxBinHz(sdrDemod.get()));
		if (!evidence || now - evidence.atMs >= SNAP_ENVELOPE_MS) {
			postRefNotice(t('sdr_locate_fallback_expired'), 2500);
			return rawHz;
		}
	}
	return target.targetHz >= S.FREQ_MIN && target.targetHz <= S.FREQ_MAX ? target.targetHz : null;
}

export function cancelSdrLocate(): void {
	const had = frozen !== null || hover !== null;
	frozen = null;
	hover = null;
	disarmStaleTimer();
	if (had) requestRender();
}

export function invalidateSdrLocateEvidence(): void {
	resetCarrierEvidence();
	cancelSdrLocate();
}

/** Dashed preview is separate from the receiver's tuned/passband indicator. */
export function renderSdrLocate(loHz: number, hiHz: number): boolean {
	let shown: LocateTarget | null = null;
	if (frozen) {
		if (!environmentChanged(frozen.environment)) {
			shown = frozen.target;
			if (shown?.mode === 'signal') {
				const now = performance.now();
				const evidence = currentCarrierEvidence(shown.carrierHz, now, signalMaxBinHz(sdrDemod.get()));
				if (evidence) shown.evidenceMs = evidence.atMs;
				if (now - (shown.evidenceMs ?? -Infinity) >= SNAP_ENVELOPE_MS) shown = null;
			}
		}
	} else {
		refreshHover();
		shown = hover?.target ?? null;
	}
	if (shown?.mode === 'free' || !S.sdrMode) shown = null;
	armStaleTimer(shown);
	const ds = shown ? JSON.stringify({ mode: shown.mode, target: shown.targetHz, carrier: shown.carrierHz }) : '';
	if (ctx.canvas.dataset.locate !== ds) ctx.canvas.dataset.locate = ds;
	if (!shown) return false;
	const p = plotRect(), x = p.x + (shown.targetHz - loHz) / ((hiHz - loHz) || 1) * p.w;
	if (!(x >= p.x) || !(x <= p.x + p.w)) return false;
	const col = canvasColors(), accent = shown.mode === 'signal' ? col.peak : col.norm;
	ctx.strokeStyle = accent;
	ctx.lineWidth = 1;
	ctx.setLineDash([4, 3]);
	ctx.beginPath();
	ctx.moveTo(x, p.y);
	ctx.lineTo(x, p.y + p.h);
	ctx.stroke();
	ctx.setLineDash([]);
	const label = `${t(`sdr_locate_${shown.mode}`)} ${formatFreqHz(shown.targetHz)}`;
	ctx.font = '11px monospace';
	ctx.textAlign = 'center';
	ctx.textBaseline = 'bottom';
	const width = ctx.measureText(label).width;
	const lx = Math.min(Math.max(x, p.x + width / 2 + 4), Math.max(p.x + width / 2 + 4, p.x + p.w - width / 2 - 4));
	ctx.fillStyle = accent;
	ctx.fillText(label, lx, p.y + p.h - 4);
	return true;
}
