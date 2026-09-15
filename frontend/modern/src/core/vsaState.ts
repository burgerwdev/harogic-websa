/**
 * VSA (vector signal analysis) client state — the single owner of every VSA parameter.
 *
 * Same discipline as `ui/sdrState.ts`: one slot per parameter (`core/params.ts`), only the
 * STATUS handler calls `confirm()`, only user actions call `set()`, `renderVsaState()` is the
 * only writer of the form controls, and nothing else keeps a copy of these values.
 *
 * Two things are VSA-specific and live here as well:
 *
 *   * **the measurement payload** — a `VSAD` frame carries one Tier 1/Tier 2 product (a symbol
 *     cloud with its ideal grid, a power-versus-time trace, a CCDF curve or a spectrogram).
 *     A constellation is a *snapshot*, so the newest frame wins and an older one is dropped
 *     (the backend uses the same latest-wins policy per frame type).
 *   * **the ambiguity report** — the recovered cloud may be rotated by a multiple of 90°, and
 *     the backend says how it was decided (`resolved_by`: a preamble, the user's rotation or
 *     nothing). The panel shows that verbatim; it never rotates silently.
 */
import { createParam, resetAll } from './params';
import { send } from './wsSend';
import { t } from './i18n';
import type { VsaFrame } from './frames';

const num = { parse: Number, serialize: String, equals: (a: number, b: number) => a === b };

// Every VSA setting is the user's preference: persisted as what the device accepted, so
// leaving the mode and coming back (or reloading) does not silently discard the geometry.
export const vsaCenterHz = createParam<number>('vsa.center', {
	fallback: 0, scope: 'vsa', persistKey: 'web-sa-vsa-center', ...num,
});
export const vsaDecimate = createParam<number>('vsa.decimate', {
	fallback: 16, scope: 'vsa', persistKey: 'web-sa-vsa-decimate', ...num,
});
/** Capture depth in samples (one FixedPoints frame). */
export const vsaDepth = createParam<number>('vsa.depth', {
	fallback: 1 << 17, scope: 'vsa', persistKey: 'web-sa-vsa-depth', ...num,
});
/** Requested Tier 1 measurement (spectrum/power/ccdf/spectrogram/constellation). */
export const vsaMeasure = createParam<string>('vsa.measure', {
	fallback: 'spectrum', scope: 'vsa', persistKey: 'web-sa-vsa-measure',
});
export const vsaModulation = createParam<string>('vsa.modulation', {
	fallback: 'qpsk', scope: 'vsa', persistKey: 'web-sa-vsa-modulation',
});
export const vsaSymbolRate = createParam<number>('vsa.symbolRate', {
	fallback: 0, scope: 'vsa', persistKey: 'web-sa-vsa-symbol-rate', ...num,
});
export const vsaRolloff = createParam<number>('vsa.rolloff', {
	fallback: 0.35, scope: 'vsa', persistKey: 'web-sa-vsa-rolloff', ...num,
});
/** The user's answer to the 4-fold carrier ambiguity, in degrees. */
export const vsaPhaseRotDeg = createParam<number>('vsa.phaseRot', {
	fallback: 0, scope: 'vsa', persistKey: 'web-sa-vsa-phase-rot', ...num,
});
/** capture | stream (a stream cannot carry the capture-only measurements). */
export const vsaView = createParam<string>('vsa.view', {
	fallback: 'capture', scope: 'vsa', persistKey: 'web-sa-vsa-view',
});

/** Measurements that need the whole capture (the backend refuses them in the stream view). */
export const VSA_CAPTURE_ONLY = ['spectrogram', 'constellation'];
/** Every measurement the backend accepts, in panel order. */
export const VSA_MEASUREMENTS = ['spectrum', 'power', 'ccdf', 'spectrogram', 'constellation'];

/** `STATUS.vsa.actual`: what the device actually configured (confirmed only). */
export const vsaActual: Record<string, number> = {};
/** `STATUS.vsa.last`: the scalar summary of the newest measurement (confirmed only). */
export const vsaMetrics: Record<string, unknown> = {};
let busy = false;
let progress = 0;

let payload: VsaFrame | null = null;
let payloadSeq = 0;

/** The newest measurement payload (null until the first VSAD frame arrives). */
export function getVsaPayload(): VsaFrame | null { return payload; }
/** Monotonic counter, so a panel can tell "same cloud" from "new cloud". */
export function vsaPayloadSeq(): number { return payloadSeq; }
export function vsaBusy(): boolean { return busy; }
export function vsaProgress(): number { return progress; }

/** A VSAD frame arrived: newest wins (a constellation is a snapshot, not a stream). */
export function setVsaPayload(frame: VsaFrame): void {
	payload = frame;
	payloadSeq++;
	drawVsaPanel();
	renderVsaMetrics();
}

export function clearVsaPayload(): void {
	payload = null;
	payloadSeq++;
	drawVsaPanel();
}

const PREF_KEYS = [
	'web-sa-vsa-center', 'web-sa-vsa-decimate', 'web-sa-vsa-depth', 'web-sa-vsa-measure',
	'web-sa-vsa-modulation', 'web-sa-vsa-symbol-rate', 'web-sa-vsa-rolloff',
	'web-sa-vsa-phase-rot', 'web-sa-vsa-view',
];

/** Drop every pending intent AND the stored preferences (Preset). */
export function resetVsaState(): void {
	resetAll('vsa');
	if (typeof localStorage === 'undefined') return;
	try {
		PREF_KEYS.forEach((key) => localStorage.removeItem(key));
	} catch {
		/* storage disabled: nothing to clear */
	}
	clearVsaPayload();
	renderVsaState();
}

/** True when the user already has VSA preferences (so entry must not re-derive them). */
export function hasStoredVsaPrefs(): boolean {
	try {
		return PREF_KEYS.some((key) => localStorage.getItem(key) !== null);
	} catch {
		return false;
	}
}

const el = (id: string) => document.getElementById(id) as HTMLInputElement | null;
const sel = (id: string) => document.getElementById(id) as HTMLSelectElement | null;

/** Write the VSA form controls from the slots (the only writer; lets a focused field be). */
export function renderVsaState(): void {
	const center = el('input-vsa-center');
	if (center && document.activeElement !== center && vsaCenterHz.get() > 0) {
		center.value = (vsaCenterHz.get() / 1e6).toFixed(6);
	}
	const dec = sel('select-vsa-decimate');
	if (dec) dec.value = String(vsaDecimate.get());
	const depth = sel('select-vsa-depth');
	if (depth) depth.value = String(vsaDepth.get());
	const measure = sel('select-vsa-measure');
	if (measure) measure.value = vsaMeasure.get();
	const mod = sel('select-vsa-modulation');
	if (mod) mod.value = vsaModulation.get();
	const rate = el('input-vsa-symbol-rate');
	if (rate && document.activeElement !== rate) {
		rate.value = vsaSymbolRate.get() > 0 ? String(vsaSymbolRate.get()) : '';
	}
	const roll = el('input-vsa-rolloff');
	if (roll && document.activeElement !== roll) roll.value = String(vsaRolloff.get());
	const rot = el('input-vsa-phase-rot');
	if (rot && document.activeElement !== rot) rot.value = String(vsaPhaseRotDeg.get());
	const view = sel('select-vsa-view');
	if (view) view.value = vsaView.get();
	// A capture-only measurement cannot ride the stream view: the backend refuses that pair,
	// so the panel disables the ones it cannot have instead of letting the user pick one and
	// read an error.
	const streaming = vsaView.get() === 'stream';
	VSA_MEASUREMENTS.forEach((kind) => {
		const option = measure?.querySelector(`option[value="${kind}"]`) as HTMLOptionElement | null;
		if (option) option.disabled = streaming && VSA_CAPTURE_ONLY.includes(kind);
	});
	renderVsaMetrics();
}

function number(id: string, fallback: number): number {
	const node = el(id);
	const value = node ? parseFloat(node.value) : NaN;
	return isFinite(value) ? value : fallback;
}

/** Frequency tolerance: sub-Hz differences are the same value. */
function sameHz(a: number, b: number): boolean { return Math.abs(a - b) < 0.5; }

/**
 * Ask for the geometry the form shows. Everything is sent in one SET_VSA so the device is
 * reconfigured once (a second configure in quick succession is the measured wedge hazard).
 */
export function applyVsa(): void {
	const centerMhz = number('input-vsa-center', 0);
	if (!(centerMhz > 0)) return;
	const center = centerMhz * 1e6;
	const decimate = Math.max(1, Math.round(number('select-vsa-decimate', vsaDecimate.get())));
	const depth = Math.max(1024, Math.round(Number(sel('select-vsa-depth')?.value ?? vsaDepth.get())));
	const measure = sel('select-vsa-measure')?.value || vsaMeasure.get();
	const modulation = sel('select-vsa-modulation')?.value || vsaModulation.get();
	const rolloff = Math.min(1, Math.max(0, number('input-vsa-rolloff', vsaRolloff.get())));
	const symbolRate = Math.max(0, number('input-vsa-symbol-rate', 0));
	const view = sel('select-vsa-view')?.value || vsaView.get();
	// Record the intent; STATUS confirms it (the slot renders from `get()` meanwhile).
	vsaCenterHz.set(center);
	vsaDecimate.set(decimate);
	vsaDepth.set(depth);
	vsaMeasure.set(measure);
	vsaModulation.set(modulation);
	vsaRolloff.set(rolloff);
	vsaSymbolRate.set(symbolRate);
	vsaView.set(view);
	if (!sameHz(center, vsaCenterHz.confirmedValue() ?? 0)) clearVsaPayload();
	renderVsaState();
	send({
		cmd: 'SET_VSA', center, decimate, depth, measure, modulation,
		rolloff, symbol_rate: symbolRate, view,
	});
}

/** The phase-rotation control: the user's answer to the 4-fold ambiguity (degrees). */
export function setVsaPhaseRotation(deg: number): void {
	const value = ((deg % 360) + 360) % 360;
	vsaPhaseRotDeg.set(value);
	renderVsaState();
	send({ cmd: 'SET_VSA', phase_rot: value });
}

// ── STATUS ────────────────────────────────────────────────────────────────────────────────

/** Confirm the VSA block of a STATUS and show what the device reported. */
export function setVsaStatus(vsa: Record<string, any> | undefined): void {
	if (!vsa) return;
	if (vsa.center !== undefined) vsaCenterHz.confirm(Number(vsa.center));
	if (vsa.decimate !== undefined) vsaDecimate.confirm(Number(vsa.decimate));
	if (vsa.depth !== undefined) vsaDepth.confirm(Number(vsa.depth));
	if (vsa.measure !== undefined) vsaMeasure.confirm(String(vsa.measure));
	if (vsa.modulation !== undefined) vsaModulation.confirm(String(vsa.modulation));
	if (vsa.rolloff !== undefined) vsaRolloff.confirm(Number(vsa.rolloff));
	if (vsa.symbol_rate !== undefined) vsaSymbolRate.confirm(Number(vsa.symbol_rate));
	if (vsa.phase_rot !== undefined) vsaPhaseRotDeg.confirm(Number(vsa.phase_rot));
	if (vsa.view !== undefined) vsaView.confirm(String(vsa.view));
	const actual = vsa.actual as Record<string, number> | undefined;
	if (actual) {
		for (const [key, value] of Object.entries(actual)) vsaActual[key] = Number(value);
	}
	busy = !!vsa.busy;
	progress = Number(vsa.progress) || 0;
	const last = vsa.last as Record<string, unknown> | undefined;
	if (last && Object.keys(last).length) {
		for (const key of Object.keys(vsaMetrics)) delete vsaMetrics[key];
		Object.assign(vsaMetrics, last);
	}
	renderVsaState();
}

// ── metrics / payload readout ─────────────────────────────────────────────────────────────

function fixed(v: unknown, digits = 3): string {
	if (typeof v !== 'number' || !isFinite(v)) return '—';
	return v.toFixed(digits);
}

function setText(id: string, text: string): void {
	const node = document.getElementById(id);
	if (node) node.textContent = text;
}

/** Write the metric readouts (the panel's only writer, same as the form controls). */
export function renderVsaMetrics(): void {
	const m = vsaMetrics;
	setText('vsa-metric-kind', String(m.kind ?? '—'));
	setText('vsa-metric-rate', typeof m.symbol_rate_used === 'number'
		? `${fixed(m.symbol_rate_used, 1)} Hz` : '—');
	setText('vsa-metric-cfo', `${fixed(m.cfo_hz, 2)} Hz`);
	setText('vsa-metric-timing', `${fixed(m.timing_samples, 3)} smp`);
	setText('vsa-metric-evm', typeof m.evm_percent === 'number'
		? `${fixed(m.evm_percent, 2)} %` : '—');
	setText('vsa-metric-mer', typeof m.mer_db === 'number' ? `${fixed(m.mer_db, 2)} dB` : '—');
	setText('vsa-metric-ser', typeof m.ser === 'number' ? fixed(m.ser, 4) : '—');
	setText('vsa-metric-symbols', typeof m.symbols_n === 'number' ? String(m.symbols_n) : '—');
	setText('vsa-metric-samples', typeof m.samples === 'number' ? String(m.samples) : '—');
	const resolved = typeof m.resolved_by === 'string' ? m.resolved_by : 'none';
	const rotation = typeof m.rotation_deg === 'number' ? m.rotation_deg : 0;
	setText('vsa-metric-ambiguity', `${resolved} · ${fixed(rotation, 1)}°`);
	const note = document.getElementById('vsa-ambiguity-note');
	if (note) {
		note.textContent = resolved === 'none'
			? t('vsa_ambiguity_unresolved')
			: resolved === 'reference' ? t('vsa_ambiguity_reference') : t('vsa_ambiguity_user');
		note.classList.toggle('warn', resolved === 'none' && (m.symbols_n as number) > 0);
	}
	const bar = document.getElementById('vsa-progress') as HTMLProgressElement | null;
	if (bar) {
		bar.style.display = busy ? '' : 'none';
		bar.value = Math.max(0, Math.min(1, progress));
	}
	const err = typeof m.error === 'string' ? m.error : '';
	setText('vsa-metric-error', err ? t(`vsa_error_${err}` as never) : '');
}

// ── drawing ───────────────────────────────────────────────────────────────────────────────

/**
 * Draw the newest payload on the panel canvas.
 *
 * One canvas for every measurement, because all four are a 2-D float32 matrix: a cloud is
 * `(points, 2)` interleaved I/Q in volts with a nominal grid to compare against, a power
 * trace and a CCDF are `(points, 2)` (x, y) curves, and a spectrogram is `(rows, bins)` in
 * relative dB. The axes are labelled with what the payload actually contains, so a cloud is
 * never mistaken for a curve.
 */
export function drawVsaPanel(): void {
	const canvas = document.getElementById('vsa-constellation') as HTMLCanvasElement | null;
	if (!canvas) return;
	const ctx = canvas.getContext('2d');
	if (!ctx) return;
	const w = canvas.width;
	const h = canvas.height;
	ctx.fillStyle = '#0b0f14';
	ctx.fillRect(0, 0, w, h);
	ctx.strokeStyle = 'rgba(120,140,160,0.35)';
	ctx.lineWidth = 1;
	ctx.beginPath();
	ctx.moveTo(w / 2, 0); ctx.lineTo(w / 2, h);
	ctx.moveTo(0, h / 2); ctx.lineTo(w, h / 2);
	ctx.stroke();
	const frame = payload;
	if (!frame || frame.rows === 0) {
		ctx.fillStyle = '#8aa0b4';
		ctx.font = '12px sans-serif';
		ctx.fillText(t('vsa_no_payload'), 8, h / 2);
		return;
	}
	if (frame.measure === 'constellation') drawCloud(ctx, frame, w, h);
	else if (frame.measure === 'spectrogram') drawSpectrogram(ctx, frame, w, h);
	else drawCurve(ctx, frame, w, h);
}

function drawGrid(ctx: CanvasRenderingContext2D, frame: VsaFrame, w: number, h: number): void {
	ctx.strokeStyle = 'rgba(150,200,255,0.55)';
	ctx.lineWidth = 1;
	for (let i = 0; i < frame.idealRows; i++) {
		const x = w / 2 + frame.ideal[i * 2] * (w / 2) * 0.85;
		const y = h / 2 - frame.ideal[i * 2 + 1] * (h / 2) * 0.85;
		ctx.strokeRect(x - 3, y - 3, 6, 6);
	}
}

function drawCloud(ctx: CanvasRenderingContext2D, frame: VsaFrame, w: number, h: number): void {
	let peak = 0;
	for (let i = 0; i < frame.data.length; i++) peak = Math.max(peak, Math.abs(frame.data[i]));
	for (let i = 0; i < frame.ideal.length; i++) peak = Math.max(peak, Math.abs(frame.ideal[i]));
	if (!(peak > 0)) peak = 1;
	const scaleX = (w / 2) * 0.85 / peak;
	const scaleY = (h / 2) * 0.85 / peak;
	drawGrid(ctx, frame, w, h);
	ctx.fillStyle = 'rgba(120,220,255,0.85)';
	for (let i = 0; i < frame.rows; i++) {
		const x = w / 2 + frame.data[i * 2] * scaleX;
		const y = h / 2 - frame.data[i * 2 + 1] * scaleY;
		ctx.fillRect(x, y, 1.2, 1.2);
	}
	ctx.fillStyle = '#8aa0b4';
	ctx.font = '11px sans-serif';
	ctx.fillText(`${frame.rows} ${t('vsa_symbols')}`, 6, h - 6);
}

function drawCurve(ctx: CanvasRenderingContext2D, frame: VsaFrame, w: number, h: number): void {
	// (x, y) rows: x is time (s) or level (dB), y is power (dBm) or probability.
	let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
	for (let i = 0; i < frame.rows; i++) {
		x0 = Math.min(x0, frame.data[i * 2]); x1 = Math.max(x1, frame.data[i * 2]);
		y0 = Math.min(y0, frame.data[i * 2 + 1]); y1 = Math.max(y1, frame.data[i * 2 + 1]);
	}
	const spanX = x1 - x0 || 1;
	const spanY = y1 - y0 || 1;
	ctx.strokeStyle = '#5ad2a0';
	ctx.lineWidth = 1.4;
	ctx.beginPath();
	for (let i = 0; i < frame.rows; i++) {
		const x = ((frame.data[i * 2] - x0) / spanX) * (w - 8) + 4;
		const y = h - 6 - ((frame.data[i * 2 + 1] - y0) / spanY) * (h - 12);
		if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
	}
	ctx.stroke();
	ctx.fillStyle = '#8aa0b4';
	ctx.font = '11px sans-serif';
	ctx.fillText(frame.measure, 6, 14);
}

function drawSpectrogram(ctx: CanvasRenderingContext2D, frame: VsaFrame, w: number, h: number): void {
	const image = ctx.createImageData(w, h);
	for (let y = 0; y < h; y++) {
		const row = Math.min(frame.rows - 1, Math.floor((y / h) * frame.rows));
		for (let x = 0; x < w; x++) {
			const col = Math.min(frame.cols - 1, Math.floor((x / w) * frame.cols));
			const db = frame.data[row * frame.cols + col];
			// Relative dB (0 = strongest bin): map [-60, 0] onto the palette.
			const v = Math.max(0, Math.min(1, (db + 60) / 60));
			const at = (y * w + x) * 4;
			image.data[at] = Math.floor(40 + v * 200);
			image.data[at + 1] = Math.floor(20 + v * 160);
			image.data[at + 2] = Math.floor(60 + v * 120);
			image.data[at + 3] = 255;
		}
	}
	ctx.putImageData(image, 0, 0);
}

// ── wiring ────────────────────────────────────────────────────────────────────────────────

/** Bind the VSA panel controls once (called from main.ts, the usual init pattern). */
export function initVsaPanel(): void {
	el('input-vsa-center')?.addEventListener('change', () => applyVsa());
	el('input-vsa-center')?.addEventListener('keydown', (ev) => {
		if ((ev as KeyboardEvent).key === 'Enter') applyVsa();
	});
	['select-vsa-decimate', 'select-vsa-depth', 'select-vsa-measure', 'select-vsa-modulation',
		'select-vsa-view'].forEach((id) => sel(id)?.addEventListener('change', () => applyVsa()));
	el('input-vsa-rolloff')?.addEventListener('change', () => applyVsa());
	el('input-vsa-symbol-rate')?.addEventListener('change', () => applyVsa());
	el('input-vsa-phase-rot')?.addEventListener('change', () => {
		setVsaPhaseRotation(number('input-vsa-phase-rot', 0));
	});
	document.querySelectorAll('[data-vsa-rot]').forEach((node) => {
		node.addEventListener('click', () => {
			const delta = Number((node as HTMLElement).dataset.vsaRot) || 0;
			setVsaPhaseRotation(vsaPhaseRotDeg.get() + delta);
		});
	});
	renderVsaState();
}
