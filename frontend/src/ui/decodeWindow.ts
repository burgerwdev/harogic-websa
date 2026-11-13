import { setDomProperty, setDomAttribute } from './domState';
import { selectIsEditing, syncSelectValue } from './selectControl';
import { cwLineFor } from './cwLine';
// One Decode surface for native analysis, protocol text, tables and retained images.
// The opener owns receiver state and subscriptions; a popup adopts the same DOM subtree.
// Floating geometry persists in viewport pixels outside the zoomed analyzer frame.
// The popup fills its own viewport without overwriting that saved floating geometry.
import { channelState } from '../core/channelState';
import { currentSdrChannel, knownSdrChannelNumbers } from './sdrChannels';
import { clearFt8Spots, clock, ft8DecoderState, ft8Spots, setFt8Channel, subscribeFt8Spots, type Ft8Spot } from '../sdr/ft8Log';
import { clearCwLog, cwLevel, cwLog, setCwChannel, subscribeCwLevel, subscribeCwLog } from '../sdr/cwLog';
import { clearDrm, drmStatus, setDrmChannel, subscribeDrm } from '../sdr/drmLog';
import {
	deleteSstvAll,
	deleteSstvSelected,
	setSstvChannel,
	setSstvModeFilter,
	setSstvSelected,
	setSstvView,
	sstvActiveSet,
	sstvCount,
	sstvModeCounts,
	sstvStatus,
	sstvVisible,
	subscribeSstv,
} from '../sdr/sstvLog';
import {
	radiofaxStatus,
	radiofaxVisiblePage,
	deleteRadiofaxPage,
	clearRadiofax,
	RADIOFAX_CHANNEL_BYTES,
	setRadiofaxChannel,
	setRadiofaxView,
	subscribeRadiofax,
	type RadiofaxEntry,
	type RadiofaxPage,
} from '../sdr/radiofaxLog';
import { sstvModeTable, modeDescriptor, modeOutputs } from '../sdr/registry';
import { clearDecoderLog, decoderLog, subscribeDecoderLog } from '../sdr/decoderLog';
import { dtmbStatus, mountDtmbPlayer, type DtmbPlayer } from './dtmbPlayer';
import { drawAudioScope as drawScope } from './audioScope';
import { renderDecoderResults } from './decoderResults';
import { createDecodePopout, type DecodePopout } from './decodePopout';
import { applyFloatingGeometry, clampFloatingGeometry } from './floatingGeometry';
import { bindWindowOpacity, initWindowOpacity } from './windowOpacity';
import './decodeResults.css';
import { ft8GeoTexts } from './ft8GeoText';
import { ft8SessionKey, markQsoRows } from './ft8Qso';
import { getLang, onLangChange, t } from '../core/i18n';
import { getUiScale, onUiScaleChange } from '../core/uiScale';
import { sdrAmSync, sdrRadiofaxAcquisition, sdrRadiofaxClockPpm, sdrRadiofaxClockRecovery,
	sdrRadiofaxIoc, sdrRadiofaxLpm, sdrRadiofaxModulation, sdrRadiofaxPhasePixels,
	sdrRadiofaxPolarity, sdrSstvMode } from './sdrState';
import type { RadiofaxAcquisition, RadiofaxClockRecovery, RadiofaxIoc, RadiofaxLpm,
	RadiofaxModulation, RadiofaxPolarity } from './sdrState';
import { ft8SnrPresentation } from '../sdr/ft8Snr';

export interface DecodeWindowHooks {
	/** Pin the SSTV decoder to a mode (index into the Rust table), or null for the VIS header. */
	onSstvMode(index: number | null): void;
	/** Select the primary AM detector without changing any other receiver parameter. */
	onAmSync?(enabled: boolean): void;
	/** Whether the current session permits changing the AM detector. */
	canAmSync?(): boolean;
	/** Apply a radiofax decoder setting or one-shot page/correction command. */
	onRadiofaxConfig?(config: RadiofaxConfigPatch): void;
}
export interface RadiofaxConfigPatch {
	radiofax_acquisition?: RadiofaxAcquisition;
	radiofax_ioc?: RadiofaxIoc;
	radiofax_lpm?: RadiofaxLpm;
	radiofax_modulation?: RadiofaxModulation;
	radiofax_polarity?: RadiofaxPolarity;
	radiofax_clock_recovery?: RadiofaxClockRecovery;
	radiofax_phase_pixels?: number;
	radiofax_clock_ppm?: number;
	radiofax_new_page?: boolean;
	radiofax_reset_correction?: boolean;
}

/// The demodulators whose output this window shows.
///
/// `sstv` is here for its picture: the station sends a still image, and the pane fills in while the
/// transmission is still running.
const DECODING_MODES = ['ft8', 'cw', 'drm', 'drmplus', 'sstv', 'radiofax', 'wfm', 'am', 'dtmb'] as const;
let dtmbPlayer: DtmbPlayer | null = null;
let dtmbHost: HTMLElement | null = null;
export type DecodeMode = string | null;

const LS_KEY = 'websa-decode-window';
// The FM pane is the tallest: a service row, RadioText, the metrics (which reflow to two rows in
// this width), the audio scope and the level row. The default has to show all of it on first open.
const DEFAULT_SIZE = { w: 460, h: 290 };
let unbindOpacity: (() => void) | undefined;
/// Whether the timestamps show UTC (off = the operator's local time). Persisted like the geometry.
let utcTime = false;
/** FT8 rows rendered; the log holds more, but a table nobody scrolls should not cost layout time. */
const MAX_ROWS = 100;
/** CW lines rendered (the log holds more; a text pane shows what fits and scrolls). */
const MAX_CW_ROWS = 120;

let hooks: DecodeWindowHooks = { onSstvMode: () => {} };
let open = false;
/** Preserve the operator's window visibility preference across demodulator switches. */
let wanted = false;
/** Receiver availability is independent of retained decoder modes and window preference. */
let active = false;
/** Which pane the window shows: the demodulator decides (null = nothing decodes right now). */
let mode: DecodeMode = null;
export type DecodeChannel = number;
let selectedChannel: DecodeChannel = 1;
const channelModes = channelState<DecodeMode>(() => null);
const sstvViews = channelState<number | null>(() => null);

export interface DecodeTiming {
	inputIndex: string;
	fractionNum: string;
	fractionDen: string;
	inputRate: number;
	delaySeconds: number;
	delayKnown: boolean;
	causalPresentation: boolean;
}
const channelTiming = channelState<DecodeTiming | null>(() => null);
const channelErrors = channelState<string | null>(() => null);
const channelErrorKinds = channelState<'receiver' | 'transport' | 'reconnecting'>(() => 'receiver');
let ingesting = false;
let pendingPaint: number | null = null;
let paintWindow: Window | null = null;
let surface: HTMLElement | null = null;
let portal: DecodePopout | null = null;
let floatingGeometry: Geometry | null = null;
const surfaceDocument = (): Document => surface?.ownerDocument ?? document;
const surfaceWindow = (): Window => surfaceDocument().defaultView ?? window;

/** Consume decoded events synchronously; painting must not delay their acknowledgement. */
export function ingestDecodeEvents(consume: () => void): void {
	ingesting = true;
	try { consume(); } finally { ingesting = false; }
}

function deferDecodePaint(): boolean {
	if (!ingesting) return false;
	if (!open) return true;
	if (pendingPaint === null) {
		paintWindow = surfaceWindow();
		pendingPaint = paintWindow.requestAnimationFrame(() => {
			pendingPaint = null;
			if (open) renderContent();
		});
	}
	return true;
}

export function setDecodeError(channel: DecodeChannel, message: string | null, kind: 'receiver' | 'transport' | 'reconnecting' = 'receiver'): void {
	channelErrors[channel] = message;
	channelErrorKinds[channel] = kind;
	if (message) { amInfo[channel] = null; amMeters[channel] = null; }
	if (channel === selectedChannel && mode === 'am') renderAm();
	if (channel === selectedChannel) renderDecodeTiming();
}

export function setDecodeTiming(channel: DecodeChannel, timing: DecodeTiming): void {
	channelTiming[channel] = timing;
	if (channel === selectedChannel) renderDecodeTiming();
}

function renderDecodeTiming(): void {
	const error = channelErrors[selectedChannel];
	const errorText = error ? t('decode_error', { message: error }) : '';
	const inline = document.getElementById('sdr-receiver-error');
	if (inline) {
		setDomProperty(inline, 'hidden', !error);
		inline.textContent = error ? t(channelErrorKinds[selectedChannel] === 'reconnecting' ? 'receiver_reconnecting'
      : channelErrorKinds[selectedChannel] === 'transport' ? 'receiver_connection_failed' : 'receiver_failed') : '';
		inline.title = errorText;
		inline.setAttribute('aria-label', errorText);
	}
  // This badge lives in the main panel. It must update even when Decode is
  // closed and its own rendering is deferred during a batch of recovered data.
  if (deferDecodePaint()) return;
  const status = element('decode-source-timing');
  const timing = channelTiming[selectedChannel];
	const timingTip = timing ? [
		t(timing.delayKnown && !timing.causalPresentation ? 'decode_timing_known' : 'decode_timing_unknown'),
		t(mode === 'drm' || mode === 'drmplus' ? 'decode_timing_drm_tip' : 'decode_timing_source_tip'),
		t('decode_timing_source', {
			index: timing.inputIndex, num: timing.fractionNum, den: timing.fractionDen, rate: timing.inputRate,
		}),
		timing.delayKnown ? t('decode_timing_delay', { delay: timing.delaySeconds.toFixed(6) }) : t('decode_timing_delay_unknown'),
		timing.causalPresentation ? t('decode_timing_causal') : '',
	].filter(Boolean).join('\n') : '';
	const tip = [errorText, timingTip].filter(Boolean).join('\n');
	if (status) {
		status.style.display = mode !== 'am' && error ? '' : 'none';
		if (status.textContent !== errorText) status.textContent = errorText;
		status.setAttribute('role', error ? 'alert' : 'status');
		status.title = tip;
	}
	const title = element('decode-window-title');
	if (title) title.title = timingTip;
	if (mode === 'am') {
		const badge = element('am-title-badge');
		if (badge) {
			badge.title = [badge.textContent, tip].filter(Boolean).join('\n');
			badge.setAttribute('role', error ? 'alert' : 'status');
			badge.setAttribute('aria-label', [badge.textContent, errorText].filter(Boolean).join(' · '));
		}
	}
}
/// The mode table the buttons were built from, so a manifest that has not loaded yet can still build
/// them later without the buttons flickering on every render.
let sstvButtons: string[] = [];
/// The history shape the select was filled from, so re-rendering does not rebuild (and reset) the
/// operator's selection while the picture is still arriving.
let sstvThumbsShape = '';
/// The mode whose picture set the pane shows (null = the live picture).
let sstvViewMode: number | null = null;
/** The channel whose destructive action is awaiting an explicit inline confirmation. */
let sstvClearChannel: DecodeChannel | null = null;
/** Radiofax view state: zoom choice and live-row following. */
type RadiofaxZoom = 'fit' | '50' | '100' | '200';
let radiofaxZoom: RadiofaxZoom = 'fit';
let radiofaxFollow = true;
let radiofaxScrollPaused = false;
let radiofaxHistoryInteracting = false;
let radiofaxSegmentId: string | null = null;
let radiofaxViewedPageId: string | null = null;
let radiofaxConfirmation: { channel: DecodeChannel; pageId: string | null } | null = null;
let radiofaxOutsideAbort: AbortController | null = null;
const RADIOFAX_EXPORT_MAX_DIMENSION = 32_767;
const RADIOFAX_EXPORT_MAX_PIXELS = 16_777_216;
/// FT8 pane view state: the message filter (null = invalid pattern, filter off) and the sorted
/// column (null = arrival order, newest first). In-memory on purpose: a view preference, not data.
let ft8FilterRe: RegExp | null = null;
let ft8FilterBad = false;
let ft8SortCol: number | null = null;
/// Storage keys for the pane's persisted view state.
const SSTV_MODE_KEY = 'web-sa-sstv-mode';
let unsubscribe: (() => void) | null = null;
let unsubscribeLevel: (() => void) | null = null;

interface Geometry {
	x: number;
	y: number;
	w: number;
	h: number;
}

function readGeometry(): Geometry | null {
	try {
		const raw = localStorage.getItem(LS_KEY);
		if (!raw) { utcTime = false; return null; }
		const parsed = JSON.parse(raw) as Partial<Geometry> & { utc?: boolean; v?: number };
		utcTime = parsed.utc === true;
		if (parsed.v !== 2 || typeof parsed.x !== 'number' || typeof parsed.y !== 'number') return null;
		return {
			x: parsed.x,
			y: parsed.y,
			w: Math.max(260, Number(parsed.w) || DEFAULT_SIZE.w),
			h: Math.max(120, Number(parsed.h) || DEFAULT_SIZE.h),
		};
	} catch {
		return null;
	}
}

function writeGeometry(geometry: Geometry): void {
	if (portal?.isPoppedOut() && floatingGeometry) geometry = floatingGeometry;
	try {
		localStorage.setItem(LS_KEY, JSON.stringify({ ...geometry, utc: utcTime, v: 2 }));
	} catch {
		/* storage disabled: the window still works for this session */
	}
}

function clampToViewport(geometry: Geometry): Geometry {
	return clampFloatingGeometry(geometry, surfaceWindow());
}

const element = (id: string) => surfaceDocument().getElementById(id) as HTMLElement | null;
const button = (id: string) => surfaceDocument().getElementById(id) as HTMLButtonElement | null;

/**
 * The window's geometry as the DOM sees it, in viewport pixels.
 *
 * `getBoundingClientRect` is the truth in a browser, but a layout-less environment (jsdom in the unit
 * tests, a hidden window after a re-init) reports zeros - and persisting a 0x0 window would then
 * restore an unusable one, so the inline style and the defaults are the fallbacks. The style values
 * are local px (the window carries the zoom), so the fallback multiplies by the scale to get back to
 * viewport pixels.
 */
function currentGeometry(win: HTMLElement): Geometry {
	const box = win.getBoundingClientRect();
	const scale = getUiScale();
	const styled = (value: string) => Math.max(0, Math.round((Number.parseFloat(value) || 0) * scale));
	return {
		x: box.width > 0 ? box.left : styled(win.style.left),
		y: box.height > 0 ? box.top : styled(win.style.top),
		w: Math.max(260, Math.round(box.width) || Math.round(win.offsetWidth * scale) || styled(win.style.width) || DEFAULT_SIZE.w),
		h: Math.max(120, Math.round(box.height) || Math.round(win.offsetHeight * scale) || styled(win.style.height) || DEFAULT_SIZE.h),
	};
}

/** Write viewport-pixel geometry into the window's styles (local px: it carries the zoom). */
function applyGeometry(win: HTMLElement, geometry: Geometry): void {
	applyFloatingGeometry(win, geometry, getUiScale());
}

/** Move without resizing: border-inclusive DOM dimensions must not feed back into
 * content-box width and height. */
function placeAt(win: HTMLElement, x: number, y: number): void {
	const scale = getUiScale();
	win.style.left = `${x / scale}px`;
	win.style.top = `${y / scale}px`;
}

/** Follow the UI scale (the window is not inside the zoomed frame). The zoom resize moves the
 * rendered box, so the geometry is re-clamped to keep the head reachable. */
onUiScaleChange(() => {
	const win = element('decode-window');
	if (!win) return;
	win.style.zoom = String(getUiScale());
	if (!portal?.isPoppedOut()) applyGeometry(win, clampToViewport(currentGeometry(win)));
});

/** Country names are language-dependent: re-render when the operator switches language. */
onLangChange(renderContent);

/** The window's floor: below this it stops being grabbable. */
const MIN_W = 260;
const MIN_H = 120;

/**
 * Wire the eight resize handles (`data-decode-rz` = n/s/e/w or a corner pair).
 *
 * Geometry stays in viewport pixels like the drag (see the coordinate note at the top) and is
 * written through `applyGeometry`, so a handle on a scaled UI moves the edge the operator grabbed
 * and nothing else. The minimum size bites on the *dragged* edge: pulling the west edge past the
 * floor keeps the east edge where it was.
 */
function wireResize(win: HTMLElement): void {
	win.querySelectorAll<HTMLElement>('[data-decode-rz]').forEach((handle) => {
		const dir = handle.dataset.decodeRz || '';
		let start: Geometry | null = null;
		let pointerX = 0;
		let pointerY = 0;
		handle.addEventListener('pointerdown', (event) => {
			if (portal?.isPoppedOut()) return;
			start = currentGeometry(win);
			pointerX = event.clientX;
			pointerY = event.clientY;
			try {
				handle.setPointerCapture(event.pointerId);
			} catch {
				/* not capturable (a synthetic event): the move handlers below still work */
			}
			event.preventDefault();
		});
		handle.addEventListener('pointermove', (event) => {
			if (!start) return;
			const dx = event.clientX - pointerX;
			const dy = event.clientY - pointerY;
			let { x, y, w, h } = start;
			if (dir.includes('e')) w = start.w + dx;
			if (dir.includes('s')) h = start.h + dy;
			if (dir.includes('w')) { w = start.w - dx; x = start.x + dx; }
			if (dir.includes('n')) { h = start.h - dy; y = start.y + dy; }
			if (w < MIN_W) { if (dir.includes('w')) x -= MIN_W - w; w = MIN_W; }
			if (h < MIN_H) { if (dir.includes('n')) y -= MIN_H - h; h = MIN_H; }
			applyGeometry(win, clampToViewport({ x, y, w, h }));
		});
		const stop = (event: PointerEvent) => {
			if (!start) return;
			start = null;
			writeGeometry(currentGeometry(win));
			try {
				handle.releasePointerCapture(event.pointerId);
			} catch {
				/* never captured */
			}
		};
		handle.addEventListener('pointerup', stop);
		handle.addEventListener('pointercancel', stop);
	});
}

// ── content ──────────────────────────────────────────────────────────────────────────────────

/** The FT8 table: one row per decode, newest first. */
function rowFor(spot: Ft8Spot, country: string, location: string): HTMLTableRowElement {
	const row = surfaceDocument().createElement('tr');
	row.className = 'ft8-row';
	row.title = `${spot.text}\n${clock(spot.at, utcTime)} ${utcTime ? 'UTC' : 'LT'}  ${(spot.hz / 1e6).toFixed(6)} MHz`;
	// The absolute frequency in MHz with 4 decimals is 100 Hz of resolution: enough to place a
	// signal on the waterfall and to tune to it, without the noise of a full-precision number.
	const key = ft8SessionKey(spot.text);
	if (key) row.dataset.qso = key;
	const snr = ft8SnrPresentation(spot.snrDb, spot.measurement);
	const cells: Array<{ text: string; className?: string; title?: string }> = [
		{ text: clock(spot.at, utcTime) },
		{ text: snr.text, className: snr.degraded ? 'ft8-snr-degraded' : undefined, title: snr.title },
		{ text: spot.offsetHz.toFixed(0) },
		{ text: (spot.hz / 1e6).toFixed(4) },
		{ text: spot.text, className: 'ft8-message' },
		{ text: country },
		{ text: location },
	];
	for (const { text, className, title } of cells) {
		const cell = surfaceDocument().createElement('td');
		cell.textContent = text;
		if (className) cell.className = className;
		if (title) cell.title = title;
		row.appendChild(cell);
	}
	return row;
}

/** The header marker for the active sort column (the ▸ glyph comes from the CSS). */
function syncFt8SortUi(): void {
	for (const th of surfaceDocument().querySelectorAll('#decode-window-table th[data-sortable]')) {
		const active = ft8SortCol !== null && Number((th as HTMLElement).dataset.col) === ft8SortCol;
		th.classList.toggle('sorted', active);
		if (active) th.setAttribute('aria-sort', 'ascending');
		else th.removeAttribute('aria-sort');
	}
}

/** Read the filter input: compile the regex, flag a pattern the engine refuses. */
function compileFt8Filter(): void {
	const input = element('decode-ft8-search') as HTMLInputElement | null;
	const text = input?.value.trim() ?? '';
	ft8FilterRe = null;
	ft8FilterBad = false;
	if (text) {
		try {
			ft8FilterRe = new RegExp(text, 'i');
		} catch {
			ft8FilterBad = true;	// filter stays off; the rows stay visible
		}
	}
	if (input) {
		input.classList.toggle('invalid', ft8FilterBad);
		input.title = t(ft8FilterBad ? 'ft8_search_bad' : 'ft8_search_tip');
	}
}

const ft8Tables = new WeakMap<HTMLElement, { spots: Ft8Spot[]; key: string; presentation: string; rows: Map<Ft8Spot, HTMLTableRowElement> }>();
function renderFt8Rows(): void {
	syncFt8SortUi();
	const body = element('decode-window-rows');
	if (!body) return;
	const spots = ft8Spots();
	const presentation = `${utcTime}:${getLang()}`, key = `${selectedChannel}:${ft8SortCol}:${ft8FilterRe}:${presentation}`;
	const previous = ft8Tables.get(body);
	// Decoder heartbeats update progress, not the retained message table.
	if (previous?.spots === spots && previous.key === key) return;
	const rows = previous?.presentation === presentation ? previous.rows : new Map<Ft8Spot, HTMLTableRowElement>();
	ft8Tables.set(body, { spots, key, presentation, rows });
	const shown = ft8FilterRe
		? spots.filter((spot) => ft8FilterRe!.test(spot.text))
		: spots.slice();
	if (ft8SortCol !== null) {
		const col = ft8SortCol;
		const key = (spot: Ft8Spot, geo: { country: string; location: string }): string | number => {
			switch (col) {
				case 0: return spot.at;
				case 1: return spot.snrDb ?? -Infinity;
				case 2: return spot.offsetHz;
				case 3: return spot.hz;
				case 4: return spot.text;
				case 5: return geo.country;
				default: return geo.location;
			}
		};
		shown.sort((a, b) => {
			const ga = ft8GeoTexts(a.text);
			const gb = ft8GeoTexts(b.text);
			const ka = key(a, ga), kb = key(b, gb);
			return (ka < kb ? -1 : ka > kb ? 1 : 0);
		});
	}
	const count = element('decode-window-count');
	if (count) count.textContent = ft8FilterRe ? `${shown.length}/${spots.length}` : String(spots.length);
	if (shown.length === 0) {
		const row = surfaceDocument().createElement('tr');
		const cell = surfaceDocument().createElement('td');
		cell.colSpan = 7;
		cell.className = 'ft8-empty';
		cell.textContent = t('ft8_waiting');
		row.appendChild(cell);
		body.replaceChildren(row); rows.clear();
		return;
	}
	const visible = shown.slice(0, MAX_ROWS), retained = new Set(visible);
	for (const spot of rows.keys()) if (!retained.has(spot)) { rows.get(spot)!.remove(); rows.delete(spot); }
	let cursor = body.firstChild;
	for (const spot of visible) {
		let row = rows.get(spot);
		if (!row) { const geo = ft8GeoTexts(spot.text); row = rowFor(spot, geo.country, geo.location); rows.set(spot, row); }
		if (row !== cursor) body.insertBefore(row, cursor);
		cursor = row.nextSibling;
	}
	while (cursor) { const next = cursor.nextSibling; cursor.remove(); cursor = next; }
}

function renderCwLines(): void {
	const pane = element('decode-window-cw');
	if (!pane) return;
	const { lines, current, marks, evictedCharacters } = cwLog();
	// Whether to follow the text: only when the operator is already at the bottom, so reading back
	// through the log is not yanked away by the next character.
	const body = element('decode-window-body');
	const follow = !body || body.scrollHeight - body.scrollTop - body.clientHeight < 24;
	pane.textContent = '';
	if (evictedCharacters) {
		const notice = surfaceDocument().createElement('div');
		notice.className = 'cw-empty';
		notice.setAttribute('role', 'status');
		notice.textContent = t('cw_history_trimmed', { count: evictedCharacters });
		pane.appendChild(notice);
	}
	if (lines.length === 0 && !current) {
		const empty = surfaceDocument().createElement('div');
		empty.className = 'cw-empty';
		empty.textContent = t('cw_waiting');
		pane.appendChild(empty);
		return;
	}
	// Two tracks, and the boundary is the sender's own pause: everything above is confirmed (the line
	// is closed and will not change), the bottom row is what is arriving right now.
	for (const line of lines.slice(-MAX_CW_ROWS)) pane.appendChild(cwLineFor(line, false, surfaceDocument(), utcTime));
	if (current) {
		const live = surfaceDocument().createElement('div');
		live.className = 'cw-live';
		live.appendChild(cwLineFor({ at: Date.now(), text: current, marks }, true, surfaceDocument(), utcTime));
		pane.appendChild(live);
	}
	if (follow && body) body.scrollTop = body.scrollHeight;
}

/**
 * The level meter and the keyed lamp.
 *
 * Separate from the lines on purpose: this runs with every audio block (tens per second) and only
 * ever writes two style properties. The scale is dBFS - the audio chain's AGC keeps a quiet band
 * around -20 dBFS, so a linear level would sit in one place all session.
 */
function renderCwMeter(): void {
	if (deferDecodePaint()) return;
	const { rms, keyed, wpm, pitchHz, afcHz } = cwLevel();
	const fill = element('cw-vu-fill');
	if (fill) {
		const db = rms > 0 ? 20 * Math.log10(rms) : -100;
		fill.style.height = `${Math.max(0, Math.min(100, ((db + 60) / 60) * 100)).toFixed(1)}%`;
	}
	const led = element('cw-led');
	if (led) led.classList.toggle('on', keyed);
	const telemetry = element('cw-telemetry');
	if (telemetry) {
		telemetry.textContent = [
			Number.isFinite(wpm) ? `${wpm!.toFixed(1)} WPM` : '',
			Number.isFinite(pitchHz) ? `${pitchHz!.toFixed(0)} Hz` : '',
			Number.isFinite(afcHz) ? `AFC ${afcHz! >= 0 ? '+' : ''}${afcHz!.toFixed(1)} Hz` : '',
		].filter(Boolean).join(' · ');
	}
}

/** Render the active pane and keep the head in step with it. */
function renderContent(): void {
	if (deferDecodePaint()) return;
	const showFt8 = mode === 'ft8';
	const showCw = mode === 'cw';
	const showDrm = mode === 'drm' || mode === 'drmplus';
	const showSstv = mode === 'sstv';
	const showRadiofax = mode === 'radiofax';
	if (!showRadiofax) closeRadiofaxMenus();
	const showWfm = mode === 'wfm';
	const showAm = mode === 'am';
	const showDtmb = mode === 'dtmb';
	const dtmb = element('decode-window-dtmb');
	if (dtmb) {
		dtmb.style.display = showDtmb ? 'flex' : 'none';
		if (showDtmb && dtmbHost !== dtmb) { dtmbPlayer?.dispose(); dtmbPlayer = mountDtmbPlayer(dtmb); dtmbHost = dtmb; }
	}
	const televisionWindow = showDtmb && open ? element('decode-window') : null;
	if (televisionWindow && !televisionWindow.dataset.dtmbSized) {
		televisionWindow.dataset.dtmbSized = 'true';
		if (!readGeometry() && !portal?.isPoppedOut()) {
			applyGeometry(televisionWindow, clampToViewport({ ...currentGeometry(televisionWindow),
				x: Math.max(0, (window.innerWidth - 640) / 2), w: 640, h: 440 }));
		}
	}
	const dtmbEntry = showDtmb ? decoderLog(selectedChannel, 'dtmb').entries.find(entry => entry.kind === 'status' && dtmbStatus(entry.data)) : null;
	dtmbPlayer?.update(dtmbStatus(dtmbEntry?.data), showDtmb && open && active && !channelErrors[selectedChannel]);
	const showGeneric = mode !== null && !(DECODING_MODES as readonly string[]).includes(mode);
	const generic = element('decode-window-results');
	if (generic) generic.style.display = showGeneric ? '' : 'none';
	const table = element('decode-window-table');
	const cw = element('decode-window-cw');
	const drm = element('decode-window-drm');
	const sstv = element('decode-window-sstv');
	const radiofax = element('decode-window-radiofax');
	const wfm = element('decode-window-wfm');
	const am = element('decode-am');
	const title = element('decode-window-title');
	const count = element('decode-window-count');
	renderDecodeTiming();
	renderSstvClearConfirmation();
	element('decode-window-body')?.classList.toggle('decode-window-body-sstv', showSstv);
	element('decode-window-body')?.classList.toggle('decode-window-body-radiofax', showRadiofax);
	element('decode-window-body')?.classList.toggle('decode-window-body-dtmb', showDtmb);
	if (table) table.style.display = showFt8 ? '' : 'none';
	if (cw) cw.style.display = showCw ? '' : 'none';
	if (drm) drm.style.display = showDrm ? '' : 'none';
	if (sstv) sstv.style.display = showSstv ? '' : 'none';
	if (radiofax) radiofax.style.display = showRadiofax ? 'flex' : 'none';
	if (wfm) wfm.style.display = showWfm ? '' : 'none';
	if (am) am.style.display = showAm ? '' : 'none';
	const meter = element('cw-meter');
	if (meter) meter.style.display = showCw ? '' : 'none';
	// The stereo badge belongs to WFM alone; every other mode hides it.
	const titleBadge = element('wfm-title-badge');
	if (titleBadge) titleBadge.style.display = showWfm ? '' : 'none';
	const amBadge = element('am-title-badge');
	if (amBadge) amBadge.style.display = showAm ? '' : 'none';
	// The SSTV waiting line lives in this head. Every other pane hides it.
	const hint = element('sstv-head-hint');
	if (hint) hint.style.display = 'none';
	// Radiofax shares the persisted local/UTC preference for first browser image receipt times.
	const utc = element('btn-decode-utc');
	if (utc) utc.style.display = showDrm || showWfm || showSstv || showAm || showDtmb ? 'none' : '';
	const clear = element('btn-decode-clear');
	// Show Clear only for panes with accumulated content.
	if (clear) clear.style.display = showDrm || showWfm || showRadiofax || showAm || showDtmb ? 'none' : '';
	if (title) title.textContent = showCw ? 'CW' : showDrm ? (mode === 'drmplus' ? 'DRM+' : 'DRM')
		: showSstv ? 'SSTV' : showRadiofax ? 'Radiofax' : showWfm ? 'FM' : showAm ? 'AM' : showFt8 ? 'FT8'
		: mode ? (modeDescriptor(mode)?.label ?? mode.toUpperCase()) : '—';
	const timeTh = element('decode-window-time-th');
	if (timeTh) timeTh.title = utcTime ? 'UTC' : 'LT';
	const searchRow = element('decode-ft8-search-row');
	if (searchRow) searchRow.style.display = showFt8 ? '' : 'none';
	const search = element('decode-ft8-search') as HTMLInputElement | null;
	if (search) search.title = t(ft8FilterBad ? 'ft8_search_bad' : 'ft8_search_tip');
	const ft8State = element('decode-ft8-state');
	if (ft8State) {
		const state = ft8DecoderState();
		ft8State.style.display = showFt8 && state ? '' : 'none';
		ft8State.textContent = state ? t('ft8_progress', { s: state.bufferedSeconds.toFixed(1), n: state.attempts, d: state.count }) : '';
		ft8State.title = state ? t('ft8_progress_tip', { n: state.bufferedSamples, e: state.epoch }) : '';
	}
	if (count) {
		count.style.display = showFt8 ? '' : 'none';
		if (!showFt8) count.textContent = String(ft8Spots().length);
	}
	if (showFt8) renderFt8Rows();	// the count text (n/total while filtering) rides with the rows
	else if (showCw) { renderCwLines(); renderCwMeter(); }
	else if (showDrm) renderDrm();
	else if (showSstv) renderSstv();
	else if (showRadiofax) renderRadiofax();
	else if (showWfm) renderWfm();
	else if (showAm) renderAm();
	else if (showGeneric && generic && mode) renderDecoderResults(generic, selectedChannel, mode, utcTime);
}

function renderSstvClearConfirmation(): void {
	const row = element('sstv-clear-confirm');
	if (row) row.style.display = sstvClearChannel !== null && active && open && mode === 'sstv' ? '' : 'none';
	const prompt = element('sstv-clear-prompt');
	if (prompt) prompt.textContent = t('sstv_confirm_clear_all');
	const confirm = button('btn-sstv-delete-all');
	if (confirm) confirm.textContent = t('sstv_delete_all');
	const cancel = button('btn-sstv-clear-cancel');
	if (cancel) cancel.textContent = t('sstv_cancel');
}

function cancelSstvClear(): void {
	sstvClearChannel = null;
	renderSstvClearConfirmation();
}

/**
 * The SSTV pane: the picture, its metadata and the mode buttons. The buttons come from the Rust mode
 * table, so the pane cannot offer a mode the decoder does not know. The detected mode stays marked
 * while a manual pin overrides it. The progress bar shows the decoder's own line count.
 */
function renderSstv(): void {
	buildSstvModeButtons();
	const entry = sstvVisible();
	const detected = sstvStatus().current?.meta.modeIndex ?? null;
	const pin = sdrSstvMode.get();
	const selectedMode = pin < 0 ? detected : pin;
	const pending = sdrSstvMode.pending();
	const counts = sstvModeCounts();
	for (const buttonEl of surfaceDocument().querySelectorAll<HTMLElement>('#sstv-mode-group [data-sstv-mode]')) {
		const value = buttonEl.dataset.sstvMode === 'auto' ? null : Number(buttonEl.dataset.sstvMode);
		buttonEl.classList.toggle('active', value === selectedMode);
		buttonEl.classList.toggle('pending', pending && value === pin);
		buttonEl.classList.toggle('detected', value !== null && value === detected);
		const badge = buttonEl.querySelector('.sstv-badge') as HTMLElement | null;
		const count = value === null ? null : counts.get(value) ?? 0;
		if (badge) {
			badge.textContent = count ? String(count) : '';
			badge.style.display = count ? '' : 'none';
		}
	}
	// The Auto toggle's own state: on = the VIS header decides.
	const autoButton = element('sstv-auto');
	if (autoButton) {
		autoButton.classList.toggle('active', pin < 0);
		autoButton.classList.toggle('pending', pending && pin < 0);
	}
	const put = (id: string, value: string): void => {
		const el = element(id);
		if (el) el.textContent = value;
	};
	put('sstv-mode-name', entry ? entry.meta.name : '—');
	put('sstv-vis', entry ? `${t('sstv_vis')} ${entry.meta.vis}` : `${t('sstv_vis')} —`);
	put('sstv-size', entry ? `${entry.meta.width}×${entry.meta.height}` : '—');
	put('sstv-time', entry ? `${clock(entry.meta.at, utcTime)} ${utcTime ? 'UTC' : 'LT'}` : '—');
	put('sstv-freq', entry && entry.meta.hz > 0 ? `${(entry.meta.hz / 1e6).toFixed(6)} MHz` : '—');
	put('sstv-count', `${sstvCount()}`);
	const progress = element('sstv-progress');
	const fill = element('sstv-progress-fill');
	const percent = entry ? entry.meta.progress : 0;
	if (fill) fill.style.setProperty('--pct', `${percent}%`);
	if (progress) progress.style.display = entry ? '' : 'none';
	put('sstv-progress-text', entry ? `${percent}%` : '');

	renderSstvThumbs();
	const canvas = element('decode-sstv-canvas') as HTMLCanvasElement | null;
	// The empty state is the title row's waiting line plus the decoder's own counters in the readout
	// row; the canvas itself stays out of the way until there is something to draw.
	const head = element('sstv-head-hint');
	if (head) head.style.display = entry ? 'none' : '';
	const decoder = sstvStatus().decoder;
	const stateChip = element('sstv-state');
	if (stateChip) {
		stateChip.style.display = entry ? 'none' : '';
		// Three empty states, because they have three different causes: no word from the worker yet, a
		// decoder that is not running at all, and one that is running but has not found a transmission.
		stateChip.textContent = decoder
			? !decoder.running
				? t('sstv_decoder_off')
				: decoder.pushes === 0
					? t('sstv_no_frames')
					: decoder.bufferedSeconds < 1
						? t('sstv_no_audio', { n: String(decoder.pushes) })
						: `${t('sstv_state', { s: decoder.bufferedSeconds.toFixed(0), n: String(decoder.attempts) })} · dec ${Math.round(decoder.level * 100)}% · src ${Math.round((decoder.srcLevel ?? 0) * 100)}%`
			: t('sstv_no_state');
		stateChip.title = decoder
			? `${t('sstv_state_tip')}\n${decoder.pushes} blocks pushed`
			: t('sstv_state_tip');
	}
	if (canvas) {
		canvas.style.display = entry ? '' : 'none';
		if (entry) drawSstvPicture(canvas, entry.picture.rgb, entry.picture.width, entry.picture.height);
	}
	const anySelected = sstvStatus().history.some((e) => e.selected) || !!sstvStatus().current?.selected;
	const save = button('btn-sstv-save');
	if (save) setDomProperty(save, 'disabled', !anySelected);
	const selectAll = button('sstv-select-all');
	if (selectAll) {
		const total = sstvStatus().history.length + (sstvStatus().current ? 1 : 0);
		const selected = sstvStatus().history.filter((e) => e.selected).length +
			(sstvStatus().current?.selected ? 1 : 0);
		// Highlight only when every stored picture is ticked; one unticked clears it.
		selectAll.classList.toggle('active', total > 0 && selected === total);
		setDomProperty(selectAll, 'disabled', total === 0);
	}
	const del = button('btn-sstv-delete');
	if (del) {
		const anySelected = sstvStatus().history.some((entry) => entry.selected) ||
			!!sstvStatus().current?.selected;
		setDomProperty(del, 'disabled', !anySelected);
	}
}

/** The Auto control projects the receiver slot; viewing history never changes the pin. */
function setSstvAuto(on: boolean): void {
	if (!active) return;
	cancelSstvClear();
	const confirmed = sdrSstvMode.confirmedValue();
	const pin = on ? null : sstvStatus().current?.meta.modeIndex ?? sstvViewMode ??
		(confirmed !== null && confirmed >= 0 ? confirmed : null);
	// With no received, viewed or confirmed mode, leaving Auto cannot invent a decoder pin.
	if (!on && pin === null) return;
	hooks.onSstvMode(pin);
	renderContent();
}

/** The thumbnail column: the active mode's stored pictures, newest first, live one first. */
/** Build the mode buttons once, from the Rust table (empty until the manifest has loaded). */
function buildSstvModeButtons(): void {
	const group = element('sstv-mode-group');
	if (!group) return;
	const table = sstvModeTable();
	if (!table || table.length === 0) return;
	const shape = table.map((mode) => `${mode.index}:${mode.name}`).join('|');
	// The shape alone is not enough: a re-init builds into a fresh DOM, where the same table has to be
	// drawn again (the cached shape made the buttons disappear after a window re-init).
	if (shape === sstvButtons.join('|') && group.childElementCount > 0) return;
	sstvButtons = table.map((mode) => `${mode.index}:${mode.name}`);
	group.textContent = '';
	for (const mode of table) {
		const buttonEl = surfaceDocument().createElement('button');
		buttonEl.className = 'btn';
		buttonEl.dataset.sstvMode = String(mode.index);
		buttonEl.dataset.sstvName = mode.name;
		buttonEl.textContent = mode.name;
		buttonEl.title = `${t('sstv_mode_tip')}\n${mode.name} · VIS ${mode.vis} · ${mode.width}×${mode.height}`;
		const badge = surfaceDocument().createElement('span');
		badge.className = 'sstv-badge';
		badge.style.display = 'none';
		buttonEl.appendChild(badge);
		group.appendChild(buttonEl);
	}
}

function renderSstvThumbs(): void {
	const host = element('sstv-thumb-list');
	if (!host) return;
	const set = sstvActiveSet();
	const status = sstvStatus();
	// Rebuild only when the set's shape changes; the live picture repaints its own thumb on a timer.
	// The view pin and the live picture's own entry are part of the shape: both decide which
	// thumbnail carries `viewed` (the pane shows the pinned entry, else the newest), so a click
	// must restyle the list even when nothing else changed.
	const viewed = status.viewId ?? set[0]?.id ?? '';
	const shape = set.map((entry) => `${entry.id}:${entry.selected ? 1 : 0}:${Math.round(entry.meta.progress / 10)}`)
		.join('|') + `#${viewed}`;
	if (shape === sstvThumbsShape && host.childElementCount > 0) return;
	sstvThumbsShape = shape;
	host.textContent = '';
	for (const entry of set) {
		const thumb = surfaceDocument().createElement('div');
		thumb.className = 'decode-sstv-thumb' + (entry.selected ? ' selected' : '') +
			(viewed === entry.id ? ' viewed' : '');
		thumb.dataset.id = entry.id;
		const tick = surfaceDocument().createElement('span');
		tick.className = 'sstv-tick';
		tick.textContent = '✓';
		thumb.appendChild(tick);
		const canvas = surfaceDocument().createElement('canvas');
		canvas.width = 64;
		canvas.height = Math.max(24, Math.round((64 * entry.picture.height) / Math.max(1, entry.picture.width)));
		thumb.appendChild(canvas);
		thumb.title = `${entry.meta.name} ${clock(entry.meta.at, utcTime)}`;
		host.appendChild(thumb);
		drawSstvPicture(canvas, entry.picture.rgb, entry.picture.width, entry.picture.height);
	}
}

/** Blit an RGB888 buffer into a canvas, sizing the canvas to the picture's true resolution. */
function drawSstvPicture(canvas: HTMLCanvasElement, rgb: Uint8Array, width: number, height: number): void {
	if (width <= 0 || height <= 0 || rgb.length < width * height * 3) return;
	if (canvas.width !== width || canvas.height !== height) {
		canvas.width = width;
		canvas.height = height;
	}
	const ctx = canvas.getContext('2d');
	if (!ctx) return;
	const image = ctx.createImageData(width, height);
	// RGB888 in, RGBA out: the decoder never produces an alpha channel, so it is set here.
	for (let source = 0, target = 0; source < width * height * 3; source += 3, target += 4) {
		image.data[target] = rgb[source];
		image.data[target + 1] = rgb[source + 1];
		image.data[target + 2] = rgb[source + 2];
		image.data[target + 3] = 255;
	}
	ctx.putImageData(image, 0, 0);
}

/** Save the picture on screen at its true resolution, not at the size the window scaled it to. */
function saveSstvPng(): void {
	// The ticked pictures, one PNG each; with nothing ticked, the picture being viewed.
	const ticked = [sstvStatus().current, ...sstvStatus().history].filter(
		(entry): entry is NonNullable<typeof entry> => !!entry && entry.selected,
	);
	const targets = ticked.length ? ticked : sstvVisible() ? [sstvVisible()!] : [];
	for (const entry of targets) {
		const canvas = surfaceDocument().createElement('canvas');
		canvas.width = entry.picture.width;
		canvas.height = entry.picture.height;
		drawSstvPicture(canvas, entry.picture.rgb, entry.picture.width, entry.picture.height);
		const stamp = new Date(entry.meta.at).toISOString().replace(/[:.]/g, '-').slice(0, 19);
		// Serial in the name keeps multi-select downloads from colliding on one timestamp.
		canvas.toBlob((blob) => {
			if (!blob) return;
			const url = URL.createObjectURL(blob);
			const anchor = surfaceDocument().createElement('a');
			anchor.href = url;
			anchor.download = `sstv-${entry.meta.name.replace(/\s+/g, '')}-${entry.meta.serial}-${stamp}.png`;
			anchor.ownerDocument.body.appendChild(anchor);
			anchor.click();
			anchor.remove();
			// Revoked on the next tick: revoking immediately can cancel the download in some browsers.
			setTimeout(() => URL.revokeObjectURL(url), 0);
		}, 'image/png');
	}
}

// The store replaces entries for every row chunk, including same-revision overlaps.
// Decoder/status updates keep the entry, so only those paints can safely be skipped.
const radiofaxCanvasEntries = new WeakMap<HTMLCanvasElement, RadiofaxEntry>();

function drawRadiofaxPicture(canvas: HTMLCanvasElement, entry: RadiofaxEntry): void {
	const { gray, width, height } = entry.picture;
	if (radiofaxCanvasEntries.get(canvas) === entry && canvas.width === width && canvas.height === height) return;
	if (canvas.width !== width) canvas.width = width;
	if (canvas.height !== height) canvas.height = height;
	const ctx = canvas.getContext('2d');
	if (!ctx) return;
	const image = ctx.createImageData(width, height);
	for (let source = 0, target = 0; source < width * height; source++, target += 4) {
		image.data[target] = gray[source];
		image.data[target + 1] = gray[source];
		image.data[target + 2] = gray[source];
		image.data[target + 3] = 255;
	}
	ctx.putImageData(image, 0, 0);
	radiofaxCanvasEntries.set(canvas, entry);
}

function radiofaxPageName(entry: RadiofaxEntry, wholePage = false): string {
	const stamp = new Date(entry.meta.at).toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
	const hz = entry.meta.hz === null ? 'unknownHz' : `${Math.round(entry.meta.hz)}Hz`;
	return `radiofax-${stamp}-${hz}-IOC${entry.meta.ioc}-${entry.meta.lpm}LPM-p${entry.meta.transmissionId}-${wholePage ? 'page' : `s${entry.meta.segmentIndex + 1}`}.png`
		.replace(/[^\w.-]+/g, '-');
}

function showRadiofaxSaveError(show: boolean, message = t('radiofax_save_failed')): void {
	const error = element('radiofax-save-error');
	if (!error) return;
	error.style.display = show ? '' : 'none';
	error.textContent = show ? message : '';
}

function radiofaxViewedSegment(page = radiofaxVisiblePage()): RadiofaxEntry | null {
	if (!page) { radiofaxSegmentId = null; radiofaxViewedPageId = null; return null; }
	if (radiofaxViewedPageId !== page.id) { radiofaxViewedPageId = page.id; radiofaxSegmentId = null; }
	const pinned = radiofaxStatus().viewId !== null || !radiofaxFollow;
	const entry = pinned ? page.segments.find((segment) => segment.id === radiofaxSegmentId) : null;
	const visible = entry ?? page.segments[page.segments.length - 1];
	radiofaxSegmentId = visible.id;
	return visible;
}

function radiofaxWholePageSafe(page: RadiofaxPage): boolean {
	const width = page.segments[0].picture.width;
	return width <= RADIOFAX_EXPORT_MAX_DIMENSION && page.lines <= RADIOFAX_EXPORT_MAX_DIMENSION
		&& width * page.lines <= RADIOFAX_EXPORT_MAX_PIXELS
		&& page.segments.every((segment, index) => segment.picture.width === width && segment.meta.segmentIndex === index);
}

function saveRadiofaxPng(wholePage = false): void {
	const page = radiofaxVisiblePage();
	const entry = radiofaxViewedSegment(page);
	showRadiofaxSaveError(false);
	if (!entry || !page) return;
	if (wholePage && !radiofaxWholePageSafe(page)) {
		showRadiofaxSaveError(true, t('radiofax_export_limit'));
		return;
	}
	const filename = radiofaxPageName(wholePage ? page.segments[0] : entry, wholePage);
	try {
		const canvas = surfaceDocument().createElement('canvas');
		if (wholePage) {
			canvas.width = entry.picture.width;
			canvas.height = page.lines;
			const context = canvas.getContext('2d');
			if (!context) throw new Error('Canvas unavailable');
			const segmentCanvas = surfaceDocument().createElement('canvas');
			let row = 0;
			for (const segment of page.segments) {
				drawRadiofaxPicture(segmentCanvas, segment);
				context.drawImage(segmentCanvas, 0, row);
				row += segment.picture.height;
			}
		} else drawRadiofaxPicture(canvas, entry);
		const activity = element('radiofax-activity');
		if (activity) { activity.textContent = t('radiofax_saving'); activity.title = filename; }
		canvas.toBlob((blob) => {
			if (!blob) { showRadiofaxSaveError(true); if (activity) activity.textContent = ''; return; }
			const url = URL.createObjectURL(blob);
			const anchor = surfaceDocument().createElement('a');
			anchor.href = url;
			anchor.download = filename;
			anchor.ownerDocument.body.appendChild(anchor);
			anchor.click();
			anchor.remove();
			if (activity) { activity.textContent = t('radiofax_saved'); activity.title = filename; }
			setTimeout(() => URL.revokeObjectURL(url), 0);
		}, 'image/png');
	} catch { showRadiofaxSaveError(true); }
}

function radiofaxReceiptTime(at: number): string {
	const date = new Date(at);
	const year = utcTime ? date.getUTCFullYear() : date.getFullYear();
	const month = (utcTime ? date.getUTCMonth() : date.getMonth()) + 1;
	const day = utcTime ? date.getUTCDate() : date.getDate();
	return `${year}-${String(month).padStart(2, '0')}-${String(day).padStart(2, '0')} ${clock(at, utcTime)} ${utcTime ? 'UTC' : 'LT'}`;
}

function renderRadiofaxHistory(): void {
	const select = element('radiofax-history') as HTMLSelectElement | null;
	if (!select || radiofaxHistoryInteracting || selectIsEditing(select)) return;
	const state = radiofaxStatus();
	const choices = [{ id: '', label: t('radiofax_follow') }, ...state.pages.map((page) => ({
		id: page.id,
		label: `${radiofaxReceiptTime(page.at)} · ${t(page.partial ? 'radiofax_stage_partial' : page.complete ? 'radiofax_stage_complete' : 'radiofax_page_open')}`,
	}))];
	const valid = new Set(choices.map((choice) => choice.id));
	for (const option of Array.from(select.options)) if (!valid.has(option.value)) option.remove();
	for (let index = 0; index < choices.length; index++) {
		const choice = choices[index];
		let option = Array.from(select.options).find((item) => item.value === choice.id);
		if (!option) { option = select.ownerDocument.createElement('option'); option.value = choice.id; }
		if (option.textContent !== choice.label) option.textContent = choice.label;
		setDomProperty(option, 'title', t('radiofax_received_tip'));
		if (select.options[index] !== option) select.insertBefore(option, select.options[index] ?? null);
	}
	const desired = state.viewId ?? '';
	if (select.value !== desired) select.value = desired;
	setDomProperty(select, 'disabled', state.pages.length === 0);
	setDomAttribute(select, 'aria-label', t('radiofax_history_tip'));
	setDomProperty(select, 'title', t('radiofax_received_tip'));
}

function closeRadiofaxMenus(except?: HTMLDetailsElement): void {
	for (const menu of surfaceDocument().querySelectorAll<HTMLDetailsElement>('#decode-window-radiofax details')) {
		if (menu === except) continue;
		menu.open = false;
		const panel = menu.querySelector<HTMLElement>('.decode-radiofax-menu-panel');
		if (panel?.hasAttribute('popover')) panel.hidePopover?.();
	}
}

function renderRadiofaxPageDetails(page: RadiofaxPage | null, entry: RadiofaxEntry | null): void {
	const state = radiofaxStatus();
	const put = (id: string, text: string): void => { const node = element(id); if (node) node.textContent = text; };
	put('radiofax-received', page ? `${t('radiofax_received')}: ${radiofaxReceiptTime(page.at)}` : '');
	const receipt = element('radiofax-received');
	if (receipt) receipt.title = t('radiofax_received_tip');
	put('radiofax-page-info', `${t('radiofax_viewed_page')}: ${page ? `${t(page.partial ? 'radiofax_stage_partial' : page.complete ? 'radiofax_stage_complete' : 'radiofax_page_open')} · ${page.segments[0].picture.width} × ${page.lines}` : '—'}`);
	put('radiofax-missing', page?.missingRows.length ? t('radiofax_missing_short', { count: page.missingRows.length }) : '');
	const missing = element('radiofax-missing');
	if (missing) missing.title = page?.missingRows.length ? t('radiofax_missing_rows', { count: page.missingRows.length }) : '';
	put('radiofax-prefix-missing', page && page.segments[0].meta.segmentIndex > 0 ? t('radiofax_prefix_missing') : '');
	put('radiofax-budget', state.retention === 'blocked' ? t('radiofax_budget_blocked') : '');
	put('radiofax-suppressed', state.currentSuppressed ? t('radiofax_current_deleted') : '');
	put('radiofax-eviction', state.evicted ? t('radiofax_evicted_short', { count: state.evicted }) : '');
	const eviction = element('radiofax-eviction');
	if (eviction) eviction.title = t('radiofax_evicted', { count: state.evicted });
	put('radiofax-resource', `${(state.bytes / 1048576).toFixed(1)} / ${RADIOFAX_CHANNEL_BYTES / 1048576} MiB · ${t('radiofax_evicted', { count: state.evicted })} · ${t('radiofax_session_only')}`);
	put('radiofax-reason', [page?.reason ? `${t('radiofax_page_reason')}: ${page.reason}` : '', state.decoder?.reason ? `${t('radiofax_rx_reason')}: ${state.decoder.reason}` : '', state.retentionReason].filter(Boolean).join(' · '));
	put('radiofax-segment', entry && page ? t('radiofax_segment', { index: entry.meta.segmentIndex + 1, count: page.segments.length }) : '—');
	element('radiofax-segments')?.classList.toggle('shown', !!page && page.segments.length > 1);
	const index = page?.segments.findIndex((segment) => segment.id === entry?.id) ?? -1;
	const previous = button('radiofax-segment-prev');
	const next = button('radiofax-segment-next');
	if (previous) setDomProperty(previous, 'disabled', index <= 0);
	if (next) setDomProperty(next, 'disabled', !page || index >= page.segments.length - 1);
	const remove = button('radiofax-delete');
	if (remove) setDomProperty(remove, 'disabled', !page);
	const whole = button('radiofax-save-page');
	if (whole) setDomProperty(whole, 'disabled', !page || !radiofaxWholePageSafe(page));
	const segment = button('radiofax-save-segment');
	if (segment) setDomProperty(segment, 'disabled', !entry);
	const save = element('radiofax-save');
	if (save) { save.setAttribute('aria-disabled', String(!entry)); save.tabIndex = entry ? 0 : -1; }
	const clear = button('radiofax-clear');
	if (clear) setDomProperty(clear, 'disabled', state.pages.length === 0);
	put('radiofax-export-limit', page && !radiofaxWholePageSafe(page) ? t('radiofax_export_limit') : '');
	const confirmation = element('radiofax-confirm');
	if (confirmation) confirmation.style.display = radiofaxConfirmation?.channel === selectedChannel ? '' : 'none';
	put('radiofax-confirm-prompt', t(radiofaxConfirmation?.pageId ? 'radiofax_confirm_delete' : 'radiofax_confirm_clear'));
}

function renderRadiofax(): void {
	if (deferDecodePaint()) return;
	const state = radiofaxStatus();
	const page = radiofaxVisiblePage();
	const entry = radiofaxViewedSegment(page);
	const decoder = state.decoder;
	const acquisition = sdrRadiofaxAcquisition.get();
	const pending = {
		acquisition: sdrRadiofaxAcquisition.pending(), ioc: sdrRadiofaxIoc.pending(), lpm: sdrRadiofaxLpm.pending(),
		modulation: sdrRadiofaxModulation.pending(), polarity: sdrRadiofaxPolarity.pending(),
		clock: sdrRadiofaxClockRecovery.pending(), phase: sdrRadiofaxPhasePixels.pending(), ppm: sdrRadiofaxClockPpm.pending(),
	};
	for (const [id, value, isPending] of [
		['radiofax-auto', 'auto', pending.acquisition],
		['radiofax-manual', 'manual', pending.acquisition],
	] as const) {
		const control = button(id);
		if (control) {
			control.classList.toggle('active', acquisition === value);
			control.classList.toggle('pending', isPending && acquisition === value);
			setDomProperty(control, 'disabled', !active);
		}
	}
	for (const [id, param, isPending] of [
		['radiofax-ioc', sdrRadiofaxIoc.get(), pending.ioc],
		['radiofax-lpm', sdrRadiofaxLpm.get(), pending.lpm],
		['radiofax-modulation', sdrRadiofaxModulation.get(), pending.modulation],
		['radiofax-polarity', sdrRadiofaxPolarity.get(), pending.polarity],
		['radiofax-clock-recovery', sdrRadiofaxClockRecovery.get(), pending.clock],
	] as const) {
		const select = element(id) as HTMLSelectElement | null;
		if (select) {
			syncSelectValue(select, String(param));
			select.classList.toggle('pending', isPending);
			setDomProperty(select, 'disabled', !active);
		}
	}
	for (const [id, value, isPending] of [
		['radiofax-phase-pixels', sdrRadiofaxPhasePixels.get(), pending.phase],
		['radiofax-clock-ppm', sdrRadiofaxClockPpm.get(), pending.ppm],
	] as const) {
		const input = element(id) as HTMLInputElement | null;
		if (input) {
			if (input.ownerDocument.activeElement !== input) input.value = String(value);
			input.classList.toggle('pending', isPending);
			setDomProperty(input, 'disabled', !active);
		}
	}
	const status = element('radiofax-state');
	if (status) {
		const stage = decoder?.stage === 'partial'
			? t(decoder.reason === 'signal_lost' ? 'radiofax_rx_signal_lost' : decoder.reason === 'discontinuity' ? 'radiofax_rx_interrupted' : 'radiofax_rx_ended')
			: decoder ? t(`radiofax_stage_${decoder.stage}`) : t('radiofax_inactive');
		status.textContent = `${t('radiofax_rx')}: ${decoder?.running
			? `${stage} · IOC ${decoder.ioc ?? '—'} · ${decoder.lpm ?? '—'} LPM · ${t(acquisition === 'manual' ? 'radiofax_manual' : 'auto')}${decoder.clockSource === 'phasing' && (decoder.clockStatus === 'locked' || decoder.clockStatus === 'tracking') ? ` · ${t('radiofax_lock_locked')}` : decoder.clockSource === 'image_content' ? ` · ${t('radiofax_lock_heuristic')}` : ''}`
			: t('radiofax_inactive')}`;
		status.title = status.textContent;
	}
	const clock = element('radiofax-clock-state');
	if (clock) clock.textContent = decoder
		? `${t(`radiofax_clock_${decoder.clockStatus}`)} · ${decoder.clockSource === 'nominal' ? t('radiofax_clock_nominal') : t(`radiofax_clock_source_${decoder.clockSource}`)} · ${t('radiofax_phase')}: ${decoder.phasePixels.toFixed(1)} · ${decoder.clockPpm.toFixed(1)} ppm · ${t('radiofax_confidence')}: ${Math.round(decoder.clockConfidence * 100)}%`
		: t('radiofax_inactive');
	const signal = element('radiofax-signal');
	if (signal) signal.textContent = `${t('radiofax_signal_rms')}: ${decoder ? decoder.signalLevel.toPrecision(4) : '—'}`;
	const carrier = element('radiofax-carrier');
	if (carrier) carrier.textContent = `${t('radiofax_carrier_ratio')}: ${decoder ? decoder.carrierCoherence.toFixed(3) : '—'}`;
	renderRadiofaxHistory();
	renderRadiofaxPageDetails(page, entry);
	const zoom = element('radiofax-zoom') as HTMLSelectElement | null;
	syncSelectValue(zoom, radiofaxZoom);
	const follow = button('radiofax-follow');
	if (follow) {
		setDomProperty(follow, 'disabled', !page);
		follow.classList.toggle('active', radiofaxFollow && state.viewId === null);
		follow.setAttribute('aria-pressed', String(radiofaxFollow && state.viewId === null));
	}
	const newPage = button('radiofax-new-page');
	if (newPage) setDomProperty(newPage, 'disabled', !active);
	const reset = button('radiofax-reset-correction');
	if (reset) {
		setDomProperty(reset, 'disabled', !active);
		reset.classList.toggle('pending', pending.phase || pending.ppm);
	}
	const canvas = element('decode-radiofax-canvas') as HTMLCanvasElement | null;
	const empty = element('radiofax-empty');
	const view = element('radiofax-view');
	if (empty) empty.style.display = entry ? 'none' : '';
	if (canvas) {
		canvas.style.display = entry ? '' : 'none';
		if (entry) {
			drawRadiofaxPicture(canvas, entry);
			if (radiofaxZoom === 'fit') canvas.style.width = '100%';
			else canvas.style.width = `${Math.max(1, Math.round(entry.picture.width * Number(radiofaxZoom) / 100))}px`;
			canvas.style.height = 'auto';
		}
	}
	if (view && radiofaxFollow && state.viewId === null) view.scrollTop = view.scrollHeight;
}

/** Render one service snapshot, not a growing history of per-symbol diagnostics. */
function renderDrm(): void {
	const { lines, snrDb, constellation, receiver } = drmStatus();
	const lockedLine = lines.find((line) => line.startsWith('locked: '));
	const locked = receiver?.locked ?? !!lockedLine;
	const station = receiver?.stationLabel ?? lines.find((line) => line.startsWith('station: '))?.slice(9);
	const counts = lines.find((line) => line.includes(' MSC frames,'))
		?.match(/^(\d+) MSC frames, (\d+) audio AUs, FAC ok (\d+) err (\d+)/);
	const search = lines.find((line) => line.startsWith('DRM searching: '));
	const debug = lines.filter((line) => line.startsWith('dbg ')).map((line) => line.slice(4)).join('\n');
	const put = (id: string, value: string): void => { const el = element(id); if (el && el.textContent !== value) el.textContent = value; };
	const stationName = station && !station.startsWith('(') ? station : t('drm_station_waiting');
	put('decode-drm-station', stationName);
	const stationEl = element('decode-drm-station');
	if (stationEl) stationEl.title = stationName;
	const searchMode = search?.match(/,\s*([A-E])\s+([\d.]+\s*kHz)/);
	const modeText = receiver?.mode ? `${t('drm_mode')} ${receiver.mode}`
		: lockedLine ? `${t('drm_mode')} ${lockedLine.slice(8).replace(', ', ' · ')}`
			: searchMode ? `${t('drm_mode')} ${searchMode[1]} · ${searchMode[2]}` : '';
	put('decode-drm-mode', modeText);
	const modeBadge = element('decode-drm-mode');
	if (modeBadge) modeBadge.style.display = modeText ? '' : 'none';
	put('decode-drm-status', locked ? t('drm_locked') : receiver || search ? t('drm_searching') : t('drm_waiting'));
	const lockBadge = element('decode-drm-lock');
	lockBadge?.classList.toggle('on', !!locked);
	if (lockBadge) lockBadge.title = search ?? '';
	const main = element('decode-drm-main');
	if (main) main.style.display = locked ? 'grid' : 'none';
	const measuredMer = lines.find((line) => line.startsWith('FAC MER '))?.match(/^FAC MER (-?\d+(?:\.\d+)?) dB/);
	const nativeMer = receiver?.measurements?.find(m => m.metric_type === 'fac_weighted_mer');
	put('decode-drm-mer', nativeMer ? nativeMer.value?.toFixed(1) ?? '—' : measuredMer?.[1] ?? '—');
	const metrics = element('decode-drm-metrics');
	if (metrics) metrics.style.display = locked ? 'grid' : 'none';
	put('decode-drm-msc', counts?.[1] ?? '—');
	put('decode-drm-aus', counts?.[2] ?? '—');
	put('decode-drm-fac-ok', receiver ? String(receiver.facOk) : counts?.[3] ?? '—');
	put('decode-drm-fac-err', receiver ? String(receiver.facErrors) : counts?.[4] ?? '—');
	element('decode-drm-fac-err')?.classList.toggle('error', (receiver?.facErrors ?? Number(counts?.[4] ?? 0)) > 0);
	const snr = element('decode-drm-snr');
	if (snr) {
		const measured = lines.find((line) => line.startsWith('FAC SNR '))?.slice(8);
		snr.textContent = snrDb === null ? '—' : measured ?? (Number.isFinite(snrDb) ? `${snrDb?.toFixed(0)} dB` : '—');
		snr.title = snr.textContent === '—' ? t('drm_snr_unavailable') : '';
	}
	let services = element('decode-drm-services');
	if (!services) {
		const host = element('decode-window-drm');
		if (host) {
			services = surfaceDocument().createElement('div');
			services.id = 'decode-drm-services';
			host.insertBefore(services, element('decode-drm-details'));
		}
	}
	if (services) {
		services.style.display = receiver ? '' : 'none';
		const rows = receiver ? [
			`SDC ${receiver.sdcOk} · Carrier ${receiver.carrierOffsetHz === null ? '—' : `${receiver.carrierOffsetHz.toFixed(1)} Hz`}`,
			...receiver.services.map((service) => [
				service.label || `Service ${service.serviceId}`,
				`ID ${service.serviceId}`,
				service.audioCoding === null ? '' : service.audioCoding === 0 ? 'AAC' : service.audioCoding === 3 ? 'xHE-AAC' : `Audio coding ${service.audioCoding}`,
				service.sampleRate === null ? '' : `${(service.sampleRate / 1000).toFixed(1)} kHz`,
				service.channels === null ? '' : `${service.channels} ch`,
			].filter(Boolean).join(' · ')),
		] : [];
		while (services.children.length > rows.length) services.lastElementChild?.remove();
		for (const [index, text] of rows.entries()) {
			let row = services.children[index];
			if (!row) { row = surfaceDocument().createElement('div'); services.appendChild(row); }
			if (row.textContent !== text) row.textContent = text;
		}
	}
	const details = element('decode-drm-details');
	if (details) details.style.display = debug ? '' : 'none';
	put('decode-drm-debug', debug);
	const plot = element('decode-window-drm-plot') as HTMLCanvasElement | null;
	if (!plot) return;
	const ctx = plot.getContext('2d');
	if (!ctx) return;
	const { width, height } = plot;
	ctx.clearRect(0, 0, width, height);
	if (!locked) return;
	const span = 1.8;
	const cx = width / 2;
	const cy = height / 2;
	const scale = Math.min(width, height) / span;
	const style = (plot.ownerDocument.defaultView ?? window).getComputedStyle(plot);
	ctx.fillStyle = style.getPropertyValue('--text').trim() || '#00cc00';
	for (const p of constellation) {
		ctx.fillRect(cx + p.re * scale - 1, cy - p.im * scale - 1, 2, 2);
	}
	ctx.strokeStyle = style.getPropertyValue('--label').trim() || '#008800';
	for (const [re, im] of [[0.7071, 0.7071], [-0.7071, 0.7071], [0.7071, -0.7071], [-0.7071, -0.7071]]) {
		ctx.beginPath();
		ctx.arc(cx + re * scale, cy - im * scale, 5, 0, Math.PI * 2);
		ctx.stroke();
	}
}

/**
 * The WFM pane: the stereo badge, carrier parameters and audio level. The values come from
 * the worker's side chain through the `wfm-info` message, stored on the last-seen object.
 */
export interface WfmInfo {
	stereo: boolean; peakDevHz: number; pilotPct: number; peakDbFs: string; peakHoldDb: string;
	rdsLocked: boolean; rdsPi: number; rdsGroups: number; ps: string; rt: string;
	carrierHz: number; pilotDevHz: number; pilotSnrDb: number;
	scopeL: Float32Array | null; scopeR: Float32Array | null;
}
const wfmInfo = channelState<WfmInfo | null>(() => null);

/** Record the worker's latest WFM info (called from the message handler in `iqStream`). */
export function setWfmInfo(info: WfmInfo | null, channel: DecodeChannel = selectedChannel): void {
	wfmInfo[channel] = info;
	// Repaint the active WFM pane when stereo/RDS information arrives.
	if (channel === selectedChannel && open && mode === 'wfm' && !deferDecodePaint()) renderWfm();
}

/** AM measurements describe real RF input and the mono audio sent to the listener. */
export interface AmInfo {
	syncEnabled: boolean; syncLocked: boolean; syncGeneration: number;
	carrierHz: number | null; carrierOffsetHz: number | null; modulationPct: number | null; inputDbFs: number | null;
	rmsDbFs: number | null; peakDbFs: number | null; scope: Float32Array | null;
}
const amInfo = channelState<AmInfo | null>(() => null);
interface AmMeters { time: number; rmsPower: number | null; peakDb: number | null }
const amMeters = channelState<AmMeters | null>(() => null);
let confirmedAmSyncGeneration: number | null = null;

/** STATUS and telemetry travel on different sockets; only the same detector generation may lock. */
export function confirmAmSyncGeneration(generation: number | null): void {
	if (generation === confirmedAmSyncGeneration) return;
	confirmedAmSyncGeneration = generation;
	amInfo[currentSdrChannel()] = null;
	amMeters[currentSdrChannel()] = null;
	if (mode === 'am') renderAm();
}

export function setAmInfo(info: AmInfo | null, channel: DecodeChannel = selectedChannel): void {
	amInfo[channel] = active && channelModes[channel] === 'am' && !channelErrors[channel]
		&& (channel !== currentSdrChannel() || (confirmedAmSyncGeneration !== null && info?.syncGeneration === confirmedAmSyncGeneration)) ? info : null;
	const accepted = amInfo[channel];
	if (!accepted) amMeters[channel] = null;
	else {
		const previous = amMeters[channel];
		const time = performance.now();
		const elapsed = previous ? Math.max(0, time - previous.time) / 1000 : 0;
		const power = accepted.rmsDbFs != null && Number.isFinite(accepted.rmsDbFs) ? 10 ** (accepted.rmsDbFs / 10) : null;
		const peak = accepted.peakDbFs != null && Number.isFinite(accepted.peakDbFs) ? accepted.peakDbFs : null;
		amMeters[channel] = {
			time,
			rmsPower: power === null ? null : previous?.rmsPower == null ? power
				: previous.rmsPower + (1 - Math.exp(-elapsed / 0.3)) * (power - previous.rmsPower),
			peakDb: peak === null ? null : previous?.peakDb == null ? peak : Math.max(peak, previous.peakDb - 20 * elapsed),
		};
	}
	if (channel === selectedChannel && open && mode === 'am' && !deferDecodePaint()) renderAm();
}

export function canToggleAmSync(): boolean {
	return active && mode === 'am' && selectedChannel === currentSdrChannel() && !channelErrors[selectedChannel]
		&& !sdrAmSync.pending() && (hooks.canAmSync?.() ?? true);
}

function renderAm(): void {
	if (deferDecodePaint()) return;
	const info = amInfo[selectedChannel];
	const confirmed = selectedChannel === currentSdrChannel() ? sdrAmSync.confirmedValue() === true : info?.syncEnabled === true;
	const pending = selectedChannel === currentSdrChannel() && sdrAmSync.pending();
	const locked = active && confirmed && !pending && info?.syncEnabled === true && info.syncLocked
		&& (selectedChannel !== currentSdrChannel() || info.syncGeneration === confirmedAmSyncGeneration) && !channelErrors[selectedChannel];
	const badge = element('am-title-badge');
	if (badge) {
		badge.textContent = t(!confirmed ? 'am_sync_off' : locked ? 'am_sync_locked' : 'am_sync_searching');
		badge.classList.toggle('on', !!locked);
	}
	const toggle = button('am-sync-toggle');
	if (toggle) {
		toggle.textContent = t(pending ? sdrAmSync.get() ? 'am_sync_enabling' : 'am_sync_disabling' : confirmed ? 'on' : 'off');
		toggle.setAttribute('aria-pressed', String(confirmed));
		toggle.setAttribute('aria-busy', String(pending));
		toggle.classList.toggle('active', confirmed);
		toggle.classList.toggle('pending', pending);
		setDomProperty(toggle, 'disabled', !canToggleAmSync());
		toggle.title = t(pending ? 'am_sync_pending_tip' : confirmed ? 'am_sync_disable' : 'am_sync_enable');
		toggle.setAttribute('aria-label', `${t('am_sync_detector')}. ${toggle.title}`);
	}
	renderDecodeTiming();
	const carrier = element('am-carrier');
	if (carrier) carrier.textContent = info?.carrierHz != null && Number.isFinite(info.carrierHz) && info.carrierHz > 0
		? `${(info.carrierHz / 1e6).toFixed(6)} MHz` : '—';
	const put = (id: string, value: number | null | undefined, unit: string): void => {
		const el = element(id);
		if (el) el.textContent = typeof value === 'number' && Number.isFinite(value) ? `${value.toFixed(1)} ${unit}` : '—';
	};
	put('am-carrier-offset', info?.carrierOffsetHz, 'Hz');
	put('am-modulation', info?.modulationPct, '%');
	put('am-input', info?.inputDbFs, 'dBFS');
	const meter = amMeters[selectedChannel];
	put('am-rms', meter?.rmsPower != null ? 10 * Math.log10(Math.max(meter.rmsPower, 1e-16)) : null, 'dBFS');
	put('am-peak', meter?.peakDb, 'dBFS');
	const modulation = element('am-modulation');
	if (modulation) modulation.title = t('am_modulation_tip');
	const scope = element('am-scope') as HTMLCanvasElement | null;
	if (scope) {
		scope.title = t('am_scope_tip');
		scope.setAttribute('aria-label', t('am_scope_tip'));
		if (info?.scope && info.scope.length > 1) drawScope(scope, [info.scope], 1);
		else scope.getContext('2d')?.clearRect(0, 0, scope.width, scope.height);
	}
	const empty = element('am-empty');
	if (empty) { empty.textContent = t('am_waiting'); empty.style.display = info ? 'none' : ''; }
}

function renderWfm(): void {
	const w = wfmInfo[selectedChannel];
	const put = (id: string, value: string): void => {
		const el = element(id);
		if (el) el.textContent = value;
	};
	const badge = element('wfm-title-badge');
	if (badge) {
		badge.textContent = w?.stereo ? 'STEREO' : 'MONO';
		badge.classList.toggle('on', !!w?.stereo);
	}
	// The station line carries the RDS state, not just a name: a station without RDS must be
	// distinguishable from one whose name has not been decoded yet. A name that is all spaces
	// means "still collecting", so it shows the pending mark rather than an empty field.
	const ps = w?.ps.replace(/ +$/g, '') ?? '';
	const rt = w?.rt.replace(/ +/g, ' ').replace(/ +$/g, '') ?? '';
	put('wfm-station', !w ? '—' : w.rdsLocked ? (ps || '…') : t('wfm_no_rds'));
	put('wfm-rt', w?.rdsLocked ? (rt || '…') : '—');
	put('wfm-rds-pi', w?.rdsLocked ? `PI ${w.rdsPi.toString(16).toUpperCase().padStart(4, '0')}` : '—');
	// The stream's own channel centre (the event's carrierHz), not the tune box's draft.
	const carrierLabel = element('wfm-carrier-label');
	if (carrierLabel) carrierLabel.textContent = t('wfm_carrier_label');
	const carrierEl = element('wfm-carrier');
	if (carrierEl) carrierEl.title = t('wfm_carrier_tip');
	put('wfm-carrier', w && w.carrierHz > 0 ? `${(w.carrierHz / 1e6).toFixed(5)} MHz` : '—');
	const devHz = w?.peakDevHz ?? 0;
	put('wfm-dev', w ? `${devHz >= 1000 ? (devHz / 1000).toFixed(1) + ' kHz' : Math.round(devHz) + ' Hz'}` : '—');
	// Pilot deviation is a station constant. The tooltip compares it to the 75 kHz
	// broadcast-FM reference; it does not use the current programme peak.
	const pilotHz = w?.pilotDevHz ?? 0;
	put('wfm-pilot', w ? `${pilotHz >= 1000 ? (pilotHz / 1000).toFixed(1) + ' kHz' : Math.round(pilotHz) + ' Hz'}` : '—');
	const pilotEl = element('wfm-pilot');
	if (pilotEl && w) {
		pilotEl.title = `${w.pilotPct.toFixed(1)}% of 75 kHz broadcast FM deviation, ${Number.isFinite(w.pilotSnrDb) ? w.pilotSnrDb.toFixed(0) + ' dB pilot contrast (SNR estimate)' : 'pilot contrast unavailable'}`;
	}
	put('wfm-audio', w ? `${w.peakDbFs} dBFS` : '—');
	const scope = element('wfm-scope') as HTMLCanvasElement | null;
	if (scope) {
		// A stereo broadcast gets two lanes (L over R); a mono station's channels are identical,
		// so one lane is the honest picture and less clutter.
		const lanes: Float32Array[] = [];
		if (w?.scopeL && w.scopeL.length > 1) lanes.push(w.scopeL);
		if (w?.stereo && w.scopeR && w.scopeR.length > 1) lanes.push(w.scopeR);
		if (lanes.length) {
			const holdLin = Math.pow(10, (Number(w?.peakHoldDb) || -200) / 20);
			drawScope(scope, lanes, holdLin);
		}
		else scope.getContext('2d')?.clearRect(0, 0, scope.width, scope.height);
	}
	const empty = element('wfm-empty');
	if (empty) empty.style.display = w ? 'none' : '';
}

/** The operator asked for the window (the toggle). */
function setOpen(next: boolean): void {
	wanted = next;
	syncOpen();
}

/** Render the state: visible only when it was asked for and the demodulator decodes. */
function syncOpen(): void {
	const available = active && channelModes[selectedChannel] !== null;
	open = active && wanted && Object.values(channelModes).some((value) => value !== null);
	if (!open || mode !== 'sstv') cancelSstvClear();
	const win = element('decode-window');
	const toggle = document.getElementById('btn-decode-window') as HTMLButtonElement | null;
	if (win) win.style.display = open ? 'flex' : 'none';
	const amToggle = button('am-sync-toggle');
	if (amToggle) setDomProperty(amToggle, 'disabled', !canToggleAmSync());
	if (toggle) {
		setDomProperty(toggle, 'disabled', !available);
		toggle.title = available ? t('decode_window_tip') : t('decode_window_needs_mode');
		toggle.textContent = open ? t('on') : t('off');
		toggle.classList.toggle('active', open);
	}
	if (!open) { closeRadiofaxMenus(); dtmbPlayer?.update(null, false); }
	if (open) {
		renderContent();
		if (!unsubscribe) unsubscribe = subscribeAll(renderContent);
		if (!unsubscribeLevel) unsubscribeLevel = subscribeCwLevel(renderCwMeter);
	} else if (unsubscribe) {
		unsubscribe();
		unsubscribe = null;
		if (unsubscribeLevel) {
			unsubscribeLevel();
			unsubscribeLevel = null;
		}
	}
}

function subscribeAll(fn: () => void): () => void {
	const offFt8 = subscribeFt8Spots(fn);
	const offCw = subscribeCwLog(fn);
	const offDrm = subscribeDrm(fn);
	const offSstv = subscribeSstv(fn);
	const offRadiofax = subscribeRadiofax(fn);
  const offGeneric = subscribeDecoderLog(channel => { if (channel === selectedChannel) fn(); });
	return () => { offFt8(); offCw(); offDrm(); offSstv(); offRadiofax(); offGeneric(); };
}

export function toggleDecodeWindow(): void {
	if (active) setOpen(!open);
}

/** Hide an inactive receiver without discarding its logs, channel modes or visibility choice. */
export function setDecodeActive(next: boolean): void {
	if (active === next) return;
	active = next;
	if (!next) {
		for (const channel of knownSdrChannelNumbers()) { amInfo[channel] = null; amMeters[channel] = null; }
		confirmedAmSyncGeneration = null;
		renderAm();
	}
	syncOpen();
}

/** Refresh slot-backed controls after STATUS confirmation, Preset or SDR entry. */
export function refreshDecodeWindow(): void {
	syncOpen();
	if (!open) renderContent();
}

/** True while the window is showing (tests and the button state). */
export function decodeWindowOpen(): boolean {
	return open;
}

/** The pane the window is showing (tests). */
export function decodeWindowMode(): DecodeMode {
	return mode;
}

/**
 * Gate the control on the demodulator: the toggle only means something for a mode that decodes.
 *
 * Called from the STATUS handler (the demod is a slot), so modes without Decode output hide the
 * window without losing the user's choice of position or the logs.
 */
export function setDecodeWindowAvailable(demod: string): void {
	setDecodeChannelMode(selectedChannel, demod);
}

/** The confirmed demodulator for one receiver, including channels that are not being viewed. */
export function setDecodeChannelMode(channel: DecodeChannel, demod: string): void {
	const next = (DECODING_MODES as readonly string[]).includes(demod) || modeOutputs(demod).some(output => output !== 'audio') ? demod : null;
	const previous = channelModes[channel];
	if (previous === next) return;
	if ((previous === 'drm' || previous === 'drmplus') && previous !== next) clearDrm(channel);
	if (previous === 'am' && previous !== next) {
		amInfo[channel] = null;
		amMeters[channel] = null;
		if (channel === selectedChannel) renderAm();
	}
	channelModes[channel] = next;
	mode = channelModes[selectedChannel];
	// Availability is also gated by the acquisition mode in syncOpen().
	syncOpen();
}

export function decodeChannel(): DecodeChannel {
	return selectedChannel;
}

/** Select a receiver's retained decoder logs and notify the socket consumer. */
export function setDecodeChannel(channel: DecodeChannel): void {
	if (channel === selectedChannel) return;
	closeRadiofaxMenus();
	cancelSstvClear();
	sstvViews[selectedChannel] = sstvViewMode;
	selectedChannel = channel;
	mode = channelModes[channel];
	sstvViewMode = sstvViews[channel];
	sstvThumbsShape = '';
	setFt8Channel(channel);
	setCwChannel(channel);
	setDrmChannel(channel);
	setSstvChannel(channel);
	setSstvModeFilter(sstvViewMode);
	setRadiofaxChannel(channel);
	radiofaxFollow = radiofaxStatus().viewId === null;
	radiofaxHistoryInteracting = false;
	radiofaxScrollPaused = false;
	radiofaxSegmentId = null;
	radiofaxViewedPageId = null;
	radiofaxConfirmation = null;
	syncOpen();
	document.dispatchEvent(new CustomEvent('websa:decode-channel', { detail: { channel } }));
}

function bindRadiofaxOutside(): void {
	radiofaxOutsideAbort?.abort();
	radiofaxOutsideAbort = new AbortController();
	surfaceDocument().addEventListener('pointerdown', (event) => {
		for (const menu of surfaceDocument().querySelectorAll<HTMLDetailsElement>('#decode-window-radiofax details[open]')) {
			if (!menu.contains(event.target as Node)) {
				menu.open = false;
				const panel = menu.querySelector<HTMLElement>('.decode-radiofax-menu-panel');
				if (panel?.hasAttribute('popover')) panel.hidePopover?.();
			}
		}
	}, { signal: radiofaxOutsideAbort.signal });
}

/** Wire the window: its buttons, its drag/resize handles and the tune callback. */
export function initDecodeWindow(next: DecodeWindowHooks): void {
	initWindowOpacity();
	hooks = next;
	const win = portal?.isPoppedOut() ? surface : document.getElementById('decode-window');
	if (!win) return;
	if (surface === win && win.dataset.decodeBound) {
		cancelSstvClear();
		radiofaxConfirmation = null;
		setOpen(false);
		return;
	}
	portal?.dispose();
	surface = win;
	win.dataset.decodeBound = 'true';
	const head = element('decode-window-head');
	bindRadiofaxOutside();
	portal = createDecodePopout(win, () => {
		dtmbPlayer?.beforeMove();
		closeRadiofaxMenus();
		radiofaxHistoryInteracting = false;
		if (pendingPaint !== null) { paintWindow?.cancelAnimationFrame(pendingPaint); pendingPaint = null; }
		if (!portal?.isPoppedOut()) floatingGeometry = currentGeometry(win);
	}, () => {
		if (!portal?.isPoppedOut() && floatingGeometry) {
			applyGeometry(win, clampToViewport(floatingGeometry));
			floatingGeometry = null;
		}
		bindRadiofaxOutside();
		renderContent();
		dtmbPlayer?.afterMove();
	});
	button('btn-decode-popout')?.addEventListener('click', () => portal?.popout());
	button('btn-decode-dock')?.addEventListener('click', () => portal?.dock());
	cancelSstvClear();
	// Only the primary receiver has controls in the product UI. The retained
	// per-channel stores still accept protocol events independently.
	element('decode-channel')?.remove();
	setDecodeChannel(1);
	button('am-sync-toggle')?.addEventListener('click', () => {
		const toggle = button('am-sync-toggle');
		if (!toggle || toggle.disabled || !canToggleAmSync()) return;
		hooks.onAmSync?.(sdrAmSync.confirmedValue() !== true);
		renderAm();
	});
	if (head && !element('decode-source-timing')) {
		const timing = surfaceDocument().createElement('span');
		timing.id = 'decode-source-timing';
		timing.className = 'count';
		head.insertBefore(timing, head.querySelector('.spacer'));
	}
	const meter = element('cw-meter');
	if (meter && !element('cw-telemetry')) {
		const telemetry = surfaceDocument().createElement('span');
		telemetry.id = 'cw-telemetry';
		meter.appendChild(telemetry);
	}
	if (!element('decode-ft8-state')) {
		const state = surfaceDocument().createElement('div');
		state.id = 'decode-ft8-state';
		state.className = 'decode-search-row';
		state.style.display = 'none';
		win.insertBefore(state, win.querySelector('.decode-resize-layer'));
	}

	const geometry = readGeometry();
	// The zoom first, so the geometry applied below is measured against the scale that renders it.
	win.style.zoom = String(getUiScale());
	if (geometry) {
		// A stored position from a bigger window (or a broken one) is pulled back into view: a window
		// the user cannot reach is worse than a moved one.
		applyGeometry(win, clampToViewport(geometry));
	} else {
		// Default geometry explicitly places the window inside the viewport.
		applyGeometry(win, clampToViewport({
			x: Math.round((window.innerWidth - DEFAULT_SIZE.w) / 2),
			y: Math.max(60, Math.round(window.innerHeight * 0.25)),
			w: DEFAULT_SIZE.w,
			h: DEFAULT_SIZE.h,
		}));
	}
	element('btn-decode-clear')?.addEventListener('click', () => {
		if (mode === 'sstv') {
			if (!active || !open) return;
			// Keep reception running while the operator decides; Clear never deletes by itself.
			sstvClearChannel = selectedChannel;
			renderSstvClearConfirmation();
			return;
		}
		if (mode === 'cw') clearCwLog();
		else if (mode === 'drm' || mode === 'drmplus') clearDrm();
		else if (mode === 'ft8') clearFt8Spots();
		else if (mode) clearDecoderLog(selectedChannel, mode);
		renderContent();
	});
	element('btn-sstv-delete-all')?.addEventListener('click', () => {
		if (sstvClearChannel !== selectedChannel || !active || !open || mode !== 'sstv') return;
		cancelSstvClear();
		deleteSstvAll();
		sstvThumbsShape = '';
		renderContent();
	});
	element('btn-sstv-clear-cancel')?.addEventListener('click', cancelSstvClear);
	element('btn-decode-close')?.addEventListener('click', () => {
		if (portal?.isPoppedOut()) portal.dock();
		else if (active) setOpen(false);
	});

	// FT8 pane: the regex filter, the sortable headers (click sorts, click again restores the
	// arrival order) and the QSO session marking on row hover. The rows body is rebuilt by
	// renderFt8Rows, so the hover marks are delegated from the table body itself.
	element('decode-ft8-search')?.addEventListener('input', () => {
		compileFt8Filter();
		renderContent();
	});
	element('decode-window-table')?.querySelector('thead')?.addEventListener('click', (event) => {
		const th = (event.target as HTMLElement).closest('th[data-sortable]');
		if (!th) return;
		const col = Number((th as HTMLElement).dataset.col);
		ft8SortCol = ft8SortCol === col ? null : col;
		syncFt8SortUi();
		renderContent();
	});
	{
		const body = element('decode-window-rows');
		body?.addEventListener('pointerover', (event) => {
			const row = (event.target as HTMLElement).closest('tr.ft8-row');
			markQsoRows(body, row);
		});
		body?.addEventListener('pointerout', () => markQsoRows(body, null));
	}

	// SSTV's own controls. The mode buttons are delegated from their group: they are built from the
	// Rust table, and a listener per button would have to be re-attached whenever that changes.
	element('sstv-mode-group')?.addEventListener('click', (event) => {
		const target = (event.target as HTMLElement).closest('[data-sstv-mode]') as HTMLElement | null;
		if (!target) return;
		const mode = Number(target.dataset.sstvMode);
		if (!Number.isFinite(mode)) return;
		cancelSstvClear();
		// A mode click shows that mode's picture set, and (with Auto off) pins the decode mode to it.
		setSstvModeFilter(mode);
		sstvViewMode = mode;
		try {
			localStorage.setItem(SSTV_MODE_KEY, String(mode));
		} catch { /* storage off: session-only view */ }
		if (active && sdrSstvMode.get() >= 0) hooks.onSstvMode(mode);
		renderContent();
	});
	element('sstv-auto')?.addEventListener('click', () => setSstvAuto(sdrSstvMode.get() >= 0));
	element('sstv-select-all')?.addEventListener('click', () => {
		const status = sstvStatus();
		const all = [status.current, ...status.history].filter(Boolean);
		const every = all.every((e) => e!.selected);
		for (const e of all) setSstvSelected(e!.id, !every);
		renderContent();
	});
	element('btn-sstv-delete')?.addEventListener('click', () => deleteSstvSelected());

	// The history filter survives reload independently of receiver configuration.
	try {
		const raw = localStorage.getItem(SSTV_MODE_KEY);
		const storedMode = raw !== null && raw.trim() !== '' ? Number(raw) : NaN;
		sstvViewMode = Number.isInteger(storedMode) && storedMode >= 0 ? storedMode : null;
	} catch { /* storage off: session-only view */ }
	setSstvModeFilter(sstvViewMode);
	element('btn-sstv-save')?.addEventListener('click', saveSstvPng);
	element('sstv-thumbs')?.addEventListener('click', (event) => {
		const thumb = (event.target as HTMLElement).closest('.decode-sstv-thumb') as HTMLElement | null;
		if (!thumb) return;
		const id = thumb.dataset.id ?? null;
		// The tick toggles the deletion selection; the picture itself views.
		if ((event.target as HTMLElement).classList.contains('sstv-tick') && id) {
			const status = sstvStatus();
			const hit = [status.current, ...status.history].find((e) => e && e.id === id);
			if (hit) setSstvSelected(id, !hit.selected);
		} else {
			setSstvView(id);
		}
		renderContent();
	});

	// Radiofax commands go through controls.ts, whose request-id queue keeps acknowledgements ordered.
	const radiofaxControlsRoot = element('decode-window-radiofax');
	if (!radiofaxControlsRoot?.dataset.controlsBound) {
	if (radiofaxControlsRoot) radiofaxControlsRoot.dataset.controlsBound = 'true';
	const radiofaxConfig = (patch: RadiofaxConfigPatch): void => {
		if (!active) return;
		const acquisition = patch.radiofax_acquisition ?? sdrRadiofaxAcquisition.get();
		const ioc = patch.radiofax_ioc ?? sdrRadiofaxIoc.get();
		const lpm = patch.radiofax_lpm ?? sdrRadiofaxLpm.get();
		if (patch.radiofax_ioc === 'auto' || patch.radiofax_lpm === 'auto') {
			patch.radiofax_acquisition = 'auto';
			if (lpm === '180') patch.radiofax_lpm = 'auto';
		} else if (patch.radiofax_acquisition === 'auto') {
			if (lpm === '180') patch.radiofax_lpm = 'auto';
		} else if (acquisition === 'manual' || lpm === '180') {
			const decoder = radiofaxStatus().decoder;
			patch.radiofax_acquisition = 'manual';
			if (ioc === 'auto') patch.radiofax_ioc = decoder?.ioc === 288 ? '288' : '576';
			if (lpm === 'auto') {
				const observed = String(decoder?.lpm ?? 120);
				patch.radiofax_lpm = (['60', '90', '120', '180', '240'].includes(observed) ? observed : '120') as RadiofaxLpm;
			}
		}
		hooks.onRadiofaxConfig?.(patch);
	};
	button('radiofax-auto')?.addEventListener('click', () => radiofaxConfig({ radiofax_acquisition: 'auto' }));
	button('radiofax-manual')?.addEventListener('click', () => radiofaxConfig({ radiofax_acquisition: 'manual' }));
	const bindRadiofaxSelect = <T extends string>(id: string, key: keyof RadiofaxConfigPatch): void => {
		element(id)?.addEventListener('change', () => {
			const value = (element(id) as HTMLSelectElement).value as T;
			radiofaxConfig({ [key]: value });
		});
	};
	bindRadiofaxSelect<RadiofaxIoc>('radiofax-ioc', 'radiofax_ioc');
	bindRadiofaxSelect<RadiofaxLpm>('radiofax-lpm', 'radiofax_lpm');
	bindRadiofaxSelect<RadiofaxModulation>('radiofax-modulation', 'radiofax_modulation');
	bindRadiofaxSelect<RadiofaxPolarity>('radiofax-polarity', 'radiofax_polarity');
	bindRadiofaxSelect<RadiofaxClockRecovery>('radiofax-clock-recovery', 'radiofax_clock_recovery');
	const bindRadiofaxNumber = (id: string, key: 'radiofax_phase_pixels' | 'radiofax_clock_ppm'): void => {
		element(id)?.addEventListener('change', () => {
			const input = element(id) as HTMLInputElement;
			const value = Number(input.value);
			if (!Number.isFinite(value)) { renderRadiofax(); return; }
			radiofaxConfig({ [key]: value });
		});
	};
	bindRadiofaxNumber('radiofax-phase-pixels', 'radiofax_phase_pixels');
	bindRadiofaxNumber('radiofax-clock-ppm', 'radiofax_clock_ppm');
	button('radiofax-new-page')?.addEventListener('click', () => radiofaxConfig({ radiofax_new_page: true }));
	button('radiofax-reset-correction')?.addEventListener('click', () => radiofaxConfig({ radiofax_reset_correction: true }));
	const faxRoot = element('decode-window-radiofax');
	if (faxRoot && !faxRoot.dataset.menusBound) {
		faxRoot.dataset.menusBound = 'true';
		for (const menu of faxRoot.querySelectorAll<HTMLDetailsElement>('details')) {
			const summary = menu.querySelector('summary');
			const panel = menu.querySelector<HTMLElement>('.decode-radiofax-menu-panel');
			if (!panel || !summary) continue;
			// Native popovers escape the canvas/window clipping while details supplies keyboard activation.
			panel.setAttribute('popover', 'manual');
			summary.addEventListener('click', () => { if (!menu.open) closeRadiofaxMenus(menu); });
			menu.addEventListener('toggle', () => {
				if (!menu.open) { if (panel.hasAttribute('popover')) panel.hidePopover?.(); return; }
				closeRadiofaxMenus(menu);
				const focused = panel.ownerDocument.activeElement as HTMLElement | null;
				panel.showPopover?.();
				if (focused?.isConnected && panel.ownerDocument.activeElement !== focused) focused.focus({ preventScroll: true });
				const box = summary.getBoundingClientRect();
				const scale = getUiScale();
				const width = panel.offsetWidth * scale;
				const height = panel.offsetHeight * scale;
				panel.style.left = `${Math.max(4, Math.min(box.right - width, surfaceWindow().innerWidth - width - 4)) / scale}px`;
				panel.style.top = `${Math.max(4, box.bottom + height + 4 <= surfaceWindow().innerHeight ? box.bottom + 2 : box.top - height - 2) / scale}px`;
			});
			menu.addEventListener('keydown', (event) => {
				if (event.key !== 'Escape' || !menu.open) return;
				event.preventDefault();
				event.stopPropagation();
				closeRadiofaxMenus();
				summary.focus();
			});
		}
	}
	element('radiofax-save')?.addEventListener('click', (event) => {
		const page = radiofaxVisiblePage();
		if (!page || page.segments.length <= 1) {
			event.preventDefault();
			if (page) saveRadiofaxPng();
		}
	});
	button('radiofax-save-segment')?.addEventListener('click', () => { saveRadiofaxPng(); closeRadiofaxMenus(); });
	button('radiofax-save-page')?.addEventListener('click', () => { saveRadiofaxPng(true); closeRadiofaxMenus(); });
	const history = element('radiofax-history') as HTMLSelectElement | null;
	for (const event of ['focus', 'pointerdown', 'keydown']) history?.addEventListener(event, () => { radiofaxHistoryInteracting = true; });
	history?.addEventListener('blur', () => {
		radiofaxHistoryInteracting = false;
		renderRadiofaxHistory();
	});
	history?.addEventListener('change', () => {
		radiofaxFollow = history.value === '';
		radiofaxScrollPaused = false;
		radiofaxSegmentId = null;
		setRadiofaxView(radiofaxFollow ? null : history.value);
		renderRadiofax();
	});
	const moveSegment = (offset: number): void => {
		const page = radiofaxVisiblePage();
		const entry = radiofaxViewedSegment(page);
		if (!page || !entry) return;
		const target = page.segments[page.segments.findIndex((segment) => segment.id === entry.id) + offset];
		if (!target) return;
		radiofaxFollow = false;
		radiofaxSegmentId = target.id;
		radiofaxScrollPaused = false;
		setRadiofaxView(page.id);
		const view = element('radiofax-view');
		if (view) view.scrollTop = 0;
		renderRadiofax();
	};
	button('radiofax-segment-prev')?.addEventListener('click', () => moveSegment(-1));
	button('radiofax-segment-next')?.addEventListener('click', () => moveSegment(1));
	const requestDelete = (pageId: string | null): void => {
		radiofaxConfirmation = { channel: selectedChannel, pageId };
		renderRadiofax();
	};
	button('radiofax-delete')?.addEventListener('click', () => { const page = radiofaxVisiblePage(); if (page) { closeRadiofaxMenus(); requestDelete(page.id); } });
	button('radiofax-clear')?.addEventListener('click', () => { closeRadiofaxMenus(); requestDelete(null); });
	button('radiofax-confirm-cancel')?.addEventListener('click', () => { radiofaxConfirmation = null; renderRadiofax(); });
	button('radiofax-confirm-delete')?.addEventListener('click', () => {
		if (!radiofaxConfirmation || radiofaxConfirmation.channel !== selectedChannel) return;
		const pageId = radiofaxConfirmation.pageId;
		radiofaxConfirmation = null;
		history?.blur();
		radiofaxHistoryInteracting = false;
		const activity = element('radiofax-activity');
		if (activity) activity.textContent = t('radiofax_deleting');
		if (pageId) deleteRadiofaxPage(pageId);
		else clearRadiofax();
		radiofaxFollow = radiofaxStatus().viewId === null;
		radiofaxSegmentId = null;
		if (activity) activity.textContent = t(pageId ? 'radiofax_deleted' : 'radiofax_cleared');
		renderRadiofax();
	});
	element('radiofax-view')?.addEventListener('scroll', () => {
		const view = element('radiofax-view');
		if (!view) return;
		const atBottom = view.scrollHeight - view.scrollTop - view.clientHeight <= 24;
		const page = radiofaxVisiblePage();
		if (!atBottom && radiofaxFollow && page) {
			radiofaxFollow = false;
			radiofaxScrollPaused = true;
			setRadiofaxView(page.id);
		} else if (atBottom && radiofaxScrollPaused && page && radiofaxStatus().current?.id === radiofaxSegmentId) {
			radiofaxFollow = true;
			radiofaxScrollPaused = false;
			setRadiofaxView(null);
		}
		const follow = button('radiofax-follow');
		if (follow) {
			follow.classList.toggle('active', radiofaxFollow && radiofaxStatus().viewId === null);
			follow.setAttribute('aria-pressed', String(radiofaxFollow && radiofaxStatus().viewId === null));
		}
	});
	element('radiofax-zoom')?.addEventListener('change', (event) => {
		radiofaxZoom = (event.target as HTMLSelectElement).value as RadiofaxZoom;
		renderRadiofax();
	});
	button('radiofax-follow')?.addEventListener('click', () => {
		const page = radiofaxVisiblePage();
		if (!page) return;
		const wasFollowing = radiofaxFollow && radiofaxStatus().viewId === null;
		radiofaxScrollPaused = false;
		if (wasFollowing) {
			radiofaxSegmentId = radiofaxViewedSegment(page)?.id ?? null;
			radiofaxFollow = false;
			setRadiofaxView(page.id);
		} else {
			radiofaxFollow = true;
			radiofaxSegmentId = null;
			setRadiofaxView(null);
			const view = element('radiofax-view');
			if (view) view.scrollTop = view.scrollHeight;
		}
		renderRadiofax();
	});
	}

	// All tool windows share this preference, including adopted popout documents.
	unbindOpacity?.();
	const slider = element('decode-window-opacity') as HTMLInputElement | null;
	unbindOpacity = slider ? bindWindowOpacity(win, slider) : undefined;

	// UTC toggle: off = the operator's local time, on = UTC. It re-renders both panes (the FT8 rows
	// and the CW line timestamps read the same flag) and persists with the geometry.
	const utcBtn = button('btn-decode-utc');
	if (utcBtn) {
		utcBtn.classList.toggle('active', utcTime);
		utcBtn.addEventListener('click', () => {
			utcTime = !utcTime;
			utcBtn.classList.toggle('active', utcTime);
			renderContent();
			writeGeometry(currentGeometry(win));
		});
	}

	if (head) {
		let dragging = false;
		let offsetX = 0;
		let offsetY = 0;
		head.addEventListener('pointerdown', (event) => {
			if (portal?.isPoppedOut()) return;
			if ((event.target as HTMLElement).closest('button, input, select')) return;
			const box = win.getBoundingClientRect();
			dragging = true;
			offsetX = event.clientX - box.left;
			offsetY = event.clientY - box.top;
			try {
				head.setPointerCapture(event.pointerId);
			} catch {
				/* not capturable (a synthetic event): the move handlers below still work */
			}
		});
		head.addEventListener('pointermove', (event) => {
			if (!dragging) return;
			// Left/top only: no size round-trip while dragging (see `placeAt`).
			const next = clampToViewport({
				x: event.clientX - offsetX,
				y: event.clientY - offsetY,
				w: currentGeometry(win).w,
				h: currentGeometry(win).h,
			});
			placeAt(win, next.x, next.y);
		});
		const stop = (event: PointerEvent) => {
			if (!dragging) return;
			dragging = false;
			writeGeometry(currentGeometry(win));
			try {
				head.releasePointerCapture(event.pointerId);
			} catch {
				/* never captured */
			}
		};
		head.addEventListener('pointerup', stop);
		head.addEventListener('pointercancel', stop);
	}

	wireResize(win);

	mode = channelModes[selectedChannel];
	// The DOM this init writes into is new, so the SSTV buttons and the thumbnails must be built again
	// there; the cached shapes would otherwise skip both.
	sstvButtons = [];
	sstvThumbsShape = '';
	radiofaxHistoryInteracting = false;
	radiofaxScrollPaused = false;
	radiofaxFollow = radiofaxStatus().viewId === null;
	radiofaxSegmentId = null;
	radiofaxViewedPageId = null;
	radiofaxConfirmation = null;
	renderContent();
	setOpen(false);
}
