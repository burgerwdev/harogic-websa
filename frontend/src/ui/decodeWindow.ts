// The decode window: one floating panel for what the demodulator decoded.
//
// FT8, CW and DRM share one window because an operator asks for the current decoded output:
//
//   * FT8 - a table of decodes, newest first, with UTC time, SNR, audio offset and frequency.
//     Clicking a row tunes to that signal;
//   * CW - the text the Morse decoder heard, line by line, live while keying;
//   * DRM - the current station and channel health, with an FAC constellation below.
//
// One toggle opens it in FT8, CW or DRM and the window shows that mode's pane. The interaction
// is the same in all three:
//
//   * draggable by its head and resizable from all four edges and corners (position, size and
//     opacity persist, like the keypad);
//   * capped, with a Clear button, so a long session cannot grow without bound.
//
// Coordinates: the window lives in <body>, outside the zoomed frame, and carries the UI scale
// itself (core/uiScale.ts), exactly like the keypad. That keeps one rule for all the geometry:
// drag/resize/clamp/persist math runs in viewport pixels - where pointer clientX/Y,
// getBoundingClientRect and window.innerWidth all agree - and the style writes convert to the
// window's local pixels by dividing by the scale.
import { clearFt8Spots, clock, ft8Spots, subscribeFt8Spots, type Ft8Spot } from '../sdr/ft8Log';
import { clearCwLog, cwLevel, cwLog, subscribeCwLevel, subscribeCwLog, CW_WEAK_SHARE,
	type CwLine, type CwMark } from '../sdr/cwLog';
import { clearDrm, drmStatus, subscribeDrm } from '../sdr/drmLog';
import {
	deleteSstvAll,
	deleteSstvSelected,
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
import { sstvModeTable } from '../sdr/registry';
import { formatLatLon, geoFor } from '../sdr/ft8Geo';
import { parseFt8Message } from '../sdr/ft8Message';
import { getLang, onLangChange, t } from '../core/i18n';
import { getUiScale, onUiScaleChange } from '../core/uiScale';

export interface DecodeWindowHooks {
	/** Tune the receiver to an absolute frequency (an FT8 row was clicked). */
	onTune(hz: number): void;
	/** Pin the SSTV decoder to a mode (index into the Rust table), or null for the VIS header. */
	onSstvMode(index: number | null): void;
}

/// The demodulators whose output this window shows.
///
/// `sstv` is here for its picture: the station sends a still image, and the pane fills in while the
/// transmission is still running.
const DECODING_MODES = ['ft8', 'cw', 'drm', 'drmplus', 'sstv'] as const;
export type DecodeMode = (typeof DECODING_MODES)[number] | null;

const LS_KEY = 'websa-decode-window';
const DEFAULT_SIZE = { w: 460, h: 240 };
/// The floor keeps the window grabbable. An invisible one would be worse than a moved one: the same
/// reason the position is clamped into the viewport rather than restored blindly.
const MIN_OPACITY = 0.35;
/// Live opacity, persisted alongside the geometry (one record, one writer).
let opacity = 1;
/// Whether the timestamps show UTC (off = the operator's local time). Persisted like the geometry.
let utcTime = false;
/** FT8 rows rendered; the log holds more, but a table nobody scrolls should not cost layout time. */
const MAX_ROWS = 100;
/** CW lines rendered (the log holds more; a text pane shows what fits and scrolls). */
const MAX_CW_ROWS = 120;

let hooks: DecodeWindowHooks = { onTune: () => {}, onSstvMode: () => {} };
let open = false;
/// The operator's choice, which survives a mode switch: leaving CW hides the window (there is
/// nothing to show) but does not forget that they wanted it, so coming back shows it again - the
/// first version cleared the choice on the way out and the window stayed hidden for the rest of the
/// session (reported as "CW stops working after switching the demodulator away and back").
let wanted = false;
/** Which pane the window shows: the demodulator decides (null = nothing decodes right now). */
let mode: DecodeMode = null;
/// The SSTV mode the operator pinned, or null while the VIS header decides.
let sstvPin: number | null = null;
/// The mode table the buttons were built from, so a manifest that has not loaded yet can still build
/// them later without the buttons flickering on every render.
let sstvButtons: string[] = [];
/// The history shape the select was filled from, so re-rendering does not rebuild (and reset) the
/// operator's selection while the picture is still arriving.
let sstvThumbsShape = '';
/// The Auto toggle: true = the VIS header picks the decode mode.
let sstvAuto = true;
/// The mode whose picture set the pane shows (null = the live picture).
let sstvViewMode: number | null = null;
/// FT8 pane view state: the message filter (null = invalid pattern, filter off) and the sorted
/// column (null = arrival order, newest first). In-memory on purpose: a view preference, not data.
let ft8FilterRe: RegExp | null = null;
let ft8FilterBad = false;
let ft8SortCol: number | null = null;
/// Storage keys for the pane's persisted view state.
const SSTV_AUTO_KEY = 'web-sa-sstv-auto';
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
		const parsed = JSON.parse(raw) as Partial<Geometry> & { opacity?: number; utc?: boolean; v?: number };
		const stored = Number(parsed.opacity);
		opacity = Number.isFinite(stored) ? Math.min(1, Math.max(MIN_OPACITY, stored)) : 1;
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
	try {
		// The opacity and UTC choice ride along: one record, one writer, so they cannot drift apart.
		localStorage.setItem(LS_KEY, JSON.stringify({ ...geometry, opacity, utc: utcTime, v: 2 }));
	} catch {
		/* storage disabled: the window still works for this session */
	}
}

function clampToViewport(geometry: Geometry): Geometry {
	// At least a head's worth of the window stays on screen, so it can always be dragged back.
	const maxX = Math.max(0, window.innerWidth - 120);
	const maxY = Math.max(0, window.innerHeight - 40);
	const maxW = Math.max(260, window.innerWidth);
	const maxH = Math.max(120, window.innerHeight);
	return {
		x: Math.min(Math.max(0, geometry.x), maxX),
		y: Math.min(Math.max(0, geometry.y), maxY),
		w: Math.min(Math.max(260, geometry.w), maxW),
		h: Math.min(Math.max(120, geometry.h), maxH),
	};
}

const element = (id: string) => document.getElementById(id) as HTMLElement | null;
const button = (id: string) => document.getElementById(id) as HTMLButtonElement | null;

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
	const scale = getUiScale();
	win.style.left = `${geometry.x / scale}px`;
	win.style.top = `${geometry.y / scale}px`;
	win.style.width = `${geometry.w / scale}px`;
	win.style.height = `${geometry.h / scale}px`;
}

/** Move the window without touching its size. A drag must not round-trip `w`/`h` through
 * `getBoundingClientRect` (the rect is border-inclusive while `style.width` on a content-box
 * element is not), which grew the window ~2 px on every pointermove - reported as "the window
 * gets bigger while I drag it". */
function placeAt(win: HTMLElement, x: number, y: number): void {
	const scale = getUiScale();
	win.style.left = `${x / scale}px`;
	win.style.top = `${y / scale}px`;
}

/** Follow the UI scale (the window is not inside the zoomed frame). The zoom resize moves the
 * rendered box, so the geometry is re-clamped to keep the head reachable. */
onUiScaleChange(() => {
	const win = document.getElementById('decode-window');
	if (!win) return;
	win.style.zoom = String(getUiScale());
	applyGeometry(win, clampToViewport(currentGeometry(win)));
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
	const row = document.createElement('tr');
	row.className = 'ft8-row';
	row.title = `${t('ft8_tune_tip')}\n${spot.text}\n${clock(spot.at, utcTime)} ${utcTime ? 'UTC' : 'LT'}  ${(spot.hz / 1e6).toFixed(6)} MHz`;
	// The absolute frequency in MHz with 4 decimals is 100 Hz of resolution: enough to place a
	// signal on the waterfall and to tune to it, without the noise of a full-precision number.
	const key = ft8SessionKey(spot.text);
	if (key) row.dataset.qso = key;
	const cells: Array<{ text: string; className?: string }> = [
		{ text: clock(spot.at, utcTime) },
		{ text: spot.snrDb.toFixed(0) },
		{ text: spot.offsetHz.toFixed(0) },
		{ text: (spot.hz / 1e6).toFixed(4) },
		{ text: spot.text, className: 'ft8-message' },
		{ text: country },
		{ text: location },
	];
	for (const { text, className } of cells) {
		const cell = document.createElement('td');
		cell.textContent = text;
		if (className) cell.className = className;
		row.appendChild(cell);
	}
	row.addEventListener('click', () => hooks.onTune(spot.hz));
	return row;
}

/** Country pair and grid position as the row displays them (shared by the sort keys). */
function ft8GeoTexts(spot: Ft8Spot): { country: string; location: string } {
	const geo = geoFor(spot.text);
	const isZh = getLang() === 'zh';
	const sender = geo.sender ? (isZh ? geo.sender.zh : geo.sender.name) : '—';
	const country = geo.receiver
		? `${sender} → ${isZh ? geo.receiver.zh : geo.receiver.name}`
		: sender;
	return { country, location: geo.location ? formatLatLon(geo.location) : '—' };
}

/** The callsign pair a decode belongs to (unordered; a CQ groups by the caller alone). */
function ft8SessionKey(text: string): string {
	const { sender, receiver } = parseFt8Message(text);
	if (!sender) return '';
	return receiver ? [sender, receiver].sort().join('↔') : sender;
}

/** Highlight the hovered decode and every decode of the same callsign pair (null clears). */
function markQsoRows(body: HTMLElement, row: Element | null): void {
	const key = row instanceof HTMLElement ? row.dataset.qso || '' : '';
	for (const peer of Array.from(body.querySelectorAll('tr.ft8-row'))) {
		const el = peer as HTMLElement;
		const pk = el.dataset.qso || '';
		// A CQ row groups by the caller alone, so it marks every decode that call took part in.
		const match = key !== '' && pk !== '' && (pk === key || (!key.includes('↔') && pk.split('↔').includes(key)));
		peer.classList.toggle('qso-focus', match && peer === row);
		peer.classList.toggle('qso-peer', match && peer !== row);
	}
}

/** The header marker for the active sort column (the ▸ glyph comes from the CSS). */
function syncFt8SortUi(): void {
	for (const th of document.querySelectorAll('#decode-window-table th[data-sortable]')) {
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

function renderFt8Rows(): void {
	syncFt8SortUi();
	const body = element('decode-window-rows');
	if (!body) return;
	const spots = ft8Spots();
	body.textContent = '';
	const shown = ft8FilterRe
		? spots.filter((spot) => ft8FilterRe!.test(spot.text))
		: spots;
	if (ft8SortCol !== null) {
		const col = ft8SortCol;
		const key = (spot: Ft8Spot, geo: { country: string; location: string }): string | number => {
			switch (col) {
				case 0: return spot.at;
				case 1: return spot.snrDb;
				case 2: return spot.offsetHz;
				case 3: return spot.hz;
				case 4: return spot.text;
				case 5: return geo.country;
				default: return geo.location;
			}
		};
		shown.sort((a, b) => {
			const ga = ft8GeoTexts(a);
			const gb = ft8GeoTexts(b);
			const ka = key(a, ga), kb = key(b, gb);
			return (ka < kb ? -1 : ka > kb ? 1 : 0);
		});
	}
	const count = element('decode-window-count');
	if (count) count.textContent = ft8FilterRe ? `${shown.length}/${spots.length}` : String(spots.length);
	if (shown.length === 0) {
		const row = document.createElement('tr');
		const cell = document.createElement('td');
		cell.colSpan = 7;
		cell.className = 'ft8-empty';
		cell.textContent = t('ft8_waiting');
		row.appendChild(cell);
		body.appendChild(row);
		return;
	}
	for (const spot of shown.slice(0, MAX_ROWS)) {
		body.appendChild(rowFor(spot, ft8GeoTexts(spot).country, ft8GeoTexts(spot).location));
	}
}

/** One CW line: the clock time it started at, then the text (the current line is marked).
 *
 * The text is built as runs rather than per character: a clean decode is one text node (the common
 * case by far), and only a doubtful character breaks the run into a span - which keeps a window full
 * of lines cheap to re-render as characters keep arriving. A word-length pause is drawn as a faint
 * dot, so "the sender is thinking" cannot be mistaken for "the page is stuck".
 */
function cwLineFor(
	line: CwLine | { at: number; text: string; marks: CwMark[] }, current: boolean,
): HTMLElement {
	const row = document.createElement('div');
	row.className = current ? 'cw-line current' : 'cw-line';
	const at = document.createElement('span');
	at.className = 'cw-time';
	at.textContent = clock(line.at, utcTime);
	const text = document.createElement('span');
	text.className = 'cw-text';
	let run = '';
	let weak = false;
	let runShare = 1;
	const flush = (): void => {
		if (!run) return;
		if (weak) {
			const span = document.createElement('span');
			span.className = 'cw-ch weak';
			span.textContent = run;
			// Per *run*, not per character: a doubtful character is rare and usually lasts a few
			// characters, and one span per character would rebuild thousands of nodes per line.
			span.title = t('cw_confidence_tip', { pct: Math.round(runShare * 100) });
			text.appendChild(span);
		} else {
			text.appendChild(document.createTextNode(run));
		}
		run = '';
		runShare = 1;
	};
	for (let i = 0; i < line.text.length; i++) {
		const mark = line.marks[i];
		// Never at the start of a line: the timestamp already says a new transmission began, and a dot
		// under it reads as stray punctuation.
		if (mark && mark.gap && !weak && i > 0) {
			flush();
			// Empty on purpose: the dot is drawn by CSS (`.cw-gap::before`), so the pause is visible
			// without ever entering the text - copying a line, or the log's own textContent, must not
			// come back with punctuation the sender never keyed.
			const gap = document.createElement('span');
			gap.className = 'cw-gap';
			gap.title = t('cw_pause_tip');
			text.appendChild(gap);
		}
		const doubtful = !!mark && mark.share < CW_WEAK_SHARE;
		if (doubtful !== weak) {
			flush();
			weak = doubtful;
		}
		if (mark && mark.share < runShare) runShare = mark.share;
		run += line.text[i];
	}
	flush();
	if (current) {
		const cursor = document.createElement('span');
		cursor.className = 'cw-cursor';
		cursor.textContent = '▌';
		text.appendChild(cursor);
	}
	row.appendChild(at);
	row.appendChild(text);
	return row;
}

function renderCwLines(): void {
	const pane = element('decode-window-cw');
	if (!pane) return;
	const { lines, current, marks } = cwLog();
	// Whether to follow the text: only when the operator is already at the bottom, so reading back
	// through the log is not yanked away by the next character.
	const body = element('decode-window-body');
	const follow = !body || body.scrollHeight - body.scrollTop - body.clientHeight < 24;
	pane.textContent = '';
	if (lines.length === 0 && !current) {
		const empty = document.createElement('div');
		empty.className = 'cw-empty';
		empty.textContent = t('cw_waiting');
		pane.appendChild(empty);
		return;
	}
	// Two tracks, and the boundary is the sender's own pause: everything above is confirmed (the line
	// is closed and will not change), the bottom row is what is arriving right now.
	for (const line of lines.slice(-MAX_CW_ROWS)) pane.appendChild(cwLineFor(line, false));
	if (current) {
		const live = document.createElement('div');
		live.className = 'cw-live';
		live.appendChild(cwLineFor({ at: Date.now(), text: current, marks }, true));
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
	const { rms, keyed } = cwLevel();
	const fill = element('cw-vu-fill');
	if (fill) {
		const db = rms > 0 ? 20 * Math.log10(rms) : -100;
		fill.style.height = `${Math.max(0, Math.min(100, ((db + 60) / 60) * 100)).toFixed(1)}%`;
	}
	const led = element('cw-led');
	if (led) led.classList.toggle('on', keyed);
}

/** Render the active pane and keep the head in step with it. */
function renderContent(): void {
	const showFt8 = mode === 'ft8';
	const showCw = mode === 'cw';
	const showDrm = mode === 'drm' || mode === 'drmplus';
	const showSstv = mode === 'sstv';
	const table = element('decode-window-table');
	const cw = element('decode-window-cw');
	const drm = element('decode-window-drm');
	const sstv = element('decode-window-sstv');
	const title = element('decode-window-title');
	const count = element('decode-window-count');
	if (table) table.style.display = showFt8 ? '' : 'none';
	if (cw) cw.style.display = showCw ? '' : 'none';
	if (drm) drm.style.display = showDrm ? '' : 'none';
	if (sstv) sstv.style.display = showSstv ? '' : 'none';
	const meter = element('cw-meter');
	if (meter) meter.style.display = showCw ? '' : 'none';
	// The SSTV waiting line lives in this head. Every other pane hides it.
	const hint = element('sstv-head-hint');
	if (hint) hint.style.display = 'none';
	const utc = element('btn-decode-utc');
	if (utc) utc.style.display = showDrm ? 'none' : '';
	const clear = element('btn-decode-clear');
	if (clear) clear.style.display = showDrm ? 'none' : '';
	if (title) title.textContent = showCw ? 'CW' : showDrm ? (mode === 'drmplus' ? 'DRM+' : 'DRM') : showSstv ? 'SSTV' : 'FT8';
	const timeTh = element('decode-window-time-th');
	if (timeTh) timeTh.title = utcTime ? 'UTC' : 'LT';
	const searchRow = element('decode-ft8-search-row');
	if (searchRow) searchRow.style.display = showFt8 ? '' : 'none';
	const search = element('decode-ft8-search') as HTMLInputElement | null;
	if (search) search.title = t(ft8FilterBad ? 'ft8_search_bad' : 'ft8_search_tip');
	if (count) {
		count.style.display = showFt8 ? '' : 'none';
		if (!showFt8) count.textContent = String(ft8Spots().length);
	}
	if (showFt8) renderFt8Rows();	// the count text (n/total while filtering) rides with the rows
	else if (showCw) renderCwLines();
	else if (showDrm) renderDrm();
	else if (showSstv) renderSstv();
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
	// Auto drives the decode mode; a manual pin (auto off) overrides it. The detected mode stays
	// marked either way, and the viewed mode's button carries the pane's own active state.
	const active = sstvAuto ? detected : sstvPin;
	const counts = sstvModeCounts();
	for (const buttonEl of document.querySelectorAll<HTMLElement>('#sstv-mode-group [data-sstv-mode]')) {
		const value = buttonEl.dataset.sstvMode === 'auto' ? null : Number(buttonEl.dataset.sstvMode);
		buttonEl.classList.toggle('active', value === active);
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
	if (autoButton) autoButton.classList.toggle('active', sstvAuto);
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
	if (save) save.disabled = !anySelected;
	const selectAll = button('sstv-select-all');
	if (selectAll) {
		const total = sstvStatus().history.length + (sstvStatus().current ? 1 : 0);
		const selected = sstvStatus().history.filter((e) => e.selected).length +
			(sstvStatus().current?.selected ? 1 : 0);
		// Highlight only when every stored picture is ticked; one unticked clears it.
		selectAll.classList.toggle('active', total > 0 && selected === total);
		selectAll.disabled = total === 0;
	}
	const del = button('btn-sstv-delete');
	if (del) {
		const anySelected = sstvStatus().history.some((entry) => entry.selected) ||
			!!sstvStatus().current?.selected;
		del.disabled = !anySelected;
	}
}

/** The Auto toggle: on = the VIS header picks the mode; off = the operator's pin. */
function setSstvAuto(on: boolean): void {
	sstvAuto = on;
	try {
		localStorage.setItem(SSTV_AUTO_KEY, on ? '1' : '0');
	} catch { /* storage off: session-only auto */ }
	if (on) {
		sstvPin = null;
	} else if (sstvStatus().current) {
		// Leaving auto pins the mode the signal last declared, so the decode does not drift.
		sstvPin = sstvStatus().current?.meta.modeIndex ?? null;
	}
	setSstvModeFilter(sstvAuto ? null : sstvPin);
	// The worker's pin follows the toggle: auto = the header decides.
	if (on) {
		workerSetSstvPin(null);
	} else if (sstvPin !== null) {
		workerSetSstvPin(sstvPin);
	}
	renderContent();
}

function workerSetSstvPin(index: number | null): void {
	hooks.onSstvMode(index);
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
		const buttonEl = document.createElement('button');
		buttonEl.className = 'btn';
		buttonEl.dataset.sstvMode = String(mode.index);
		buttonEl.dataset.sstvName = mode.name;
		buttonEl.textContent = mode.name;
		buttonEl.title = `${t('sstv_mode_tip')}\n${mode.name} · VIS ${mode.vis} · ${mode.width}×${mode.height}`;
		const badge = document.createElement('span');
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
	// Rebuild only when the set's shape changes; the live picture repaints its own thumb on a timer.
	const shape = set.map((entry) => `${entry.id}:${entry.selected ? 1 : 0}:${Math.round(entry.meta.progress / 10)}`).join('|');
	if (shape === sstvThumbsShape && host.childElementCount > 0) return;
	sstvThumbsShape = shape;
	host.textContent = '';
	for (const entry of set) {
		const thumb = document.createElement('div');
		thumb.className = 'decode-sstv-thumb' + (entry.selected ? ' selected' : '') +
			(sstvStatus().viewId === entry.id ? ' viewed' : '');
		thumb.dataset.id = entry.id;
		const tick = document.createElement('span');
		tick.className = 'sstv-tick';
		tick.textContent = '✓';
		thumb.appendChild(tick);
		const canvas = document.createElement('canvas');
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
		const canvas = document.createElement('canvas');
		canvas.width = entry.picture.width;
		canvas.height = entry.picture.height;
		drawSstvPicture(canvas, entry.picture.rgb, entry.picture.width, entry.picture.height);
		const stamp = new Date(entry.meta.at).toISOString().replace(/[:.]/g, '-').slice(0, 19);
		// Serial in the name keeps multi-select downloads from colliding on one timestamp.
		canvas.toBlob((blob) => {
			if (!blob) return;
			const url = URL.createObjectURL(blob);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = `sstv-${entry.meta.name.replace(/\s+/g, '')}-${entry.meta.serial}-${stamp}.png`;
			document.body.appendChild(anchor);
			anchor.click();
			anchor.remove();
			// Revoked on the next tick: revoking immediately can cancel the download in some browsers.
			setTimeout(() => URL.revokeObjectURL(url), 0);
		}, 'image/png');
	}
}

/** Render one service snapshot, not a growing history of per-symbol diagnostics. */
function renderDrm(): void {
	const { lines, snrDb, constellation } = drmStatus();
	const locked = lines.find((line) => line.startsWith('locked: '));
	const station = lines.find((line) => line.startsWith('station: '))?.slice(9);
	const counts = lines.find((line) => line.includes(' MSC frames,'))
		?.match(/^(\d+) MSC frames, (\d+) audio AUs, FAC ok (\d+) err (\d+)/);
	const search = lines.find((line) => line.startsWith('DRM searching: '));
	const debug = lines.filter((line) => line.startsWith('dbg ')).map((line) => line.slice(4)).join('\n');
	const put = (id: string, value: string): void => { const el = element(id); if (el) el.textContent = value; };
	const stationName = station && !station.startsWith('(') ? station : t('drm_station_waiting');
	put('decode-drm-station', stationName);
	const stationEl = element('decode-drm-station');
	if (stationEl) stationEl.title = stationName;
	const searchMode = search?.match(/,\s*([A-E])\s+([\d.]+\s*kHz)/);
	const modeText = locked ? `${t('drm_mode')} ${locked.slice(8).replace(', ', ' · ')}`
		: searchMode ? `${t('drm_mode')} ${searchMode[1]} · ${searchMode[2]}` : '';
	put('decode-drm-mode', modeText);
	const modeBadge = element('decode-drm-mode');
	if (modeBadge) modeBadge.style.display = modeText ? '' : 'none';
	put('decode-drm-status', locked ? t('drm_locked') : search ? t('drm_searching') : t('drm_waiting'));
	const lockBadge = element('decode-drm-lock');
	lockBadge?.classList.toggle('on', !!locked);
	if (lockBadge) lockBadge.title = search ?? '';
	const main = element('decode-drm-main');
	if (main) main.style.display = locked ? 'grid' : 'none';
	const measuredMer = lines.find((line) => line.startsWith('FAC MER '))?.match(/^FAC MER (-?\d+(?:\.\d+)?) dB/);
	put('decode-drm-mer', measuredMer?.[1] ?? '—');
	const metrics = element('decode-drm-metrics');
	if (metrics) metrics.style.display = locked ? 'grid' : 'none';
	put('decode-drm-msc', counts?.[1] ?? '—');
	put('decode-drm-aus', counts?.[2] ?? '—');
	put('decode-drm-fac-ok', counts?.[3] ?? '—');
	put('decode-drm-fac-err', counts?.[4] ?? '—');
	element('decode-drm-fac-err')?.classList.toggle('error', Number(counts?.[4] ?? 0) > 0);
	const snr = element('decode-drm-snr');
	if (snr) {
		const measured = lines.find((line) => line.startsWith('FAC SNR '))?.slice(8);
		snr.textContent = measured ?? (Number.isFinite(snrDb) ? `${snrDb?.toFixed(0)} dB` : '—');
		snr.title = snr.textContent === '—' ? t('drm_snr_unavailable') : '';
	}
	const details = element('decode-drm-details');
	if (details) details.style.display = debug ? '' : 'none';
	put('decode-drm-debug', debug);
	const plot = element('decode-window-drm-plot') as HTMLCanvasElement | null;
	if (!plot || !locked) return;
	const ctx = plot.getContext('2d');
	if (!ctx) return;
	const { width, height } = plot;
	ctx.clearRect(0, 0, width, height);
	const span = 1.8;
	const cx = width / 2;
	const cy = height / 2;
	const scale = Math.min(width, height) / span;
	const style = getComputedStyle(plot);
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

/** The operator asked for the window (the toggle). */
function setOpen(next: boolean): void {
	wanted = next;
	syncOpen();
}

/** Render the state: visible only when it was asked for and the demodulator decodes. */
function syncOpen(): void {
	open = wanted && mode !== null;
	const win = element('decode-window');
	const toggle = button('btn-decode-window');
	if (win) win.style.display = open ? 'flex' : 'none';
	if (toggle) {
		toggle.textContent = open ? t('on') : t('off');
		toggle.classList.toggle('active', open);
	}
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
	return () => { offFt8(); offCw(); offDrm(); offSstv(); };
}

export function toggleDecodeWindow(): void {
	setOpen(!open);
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
 * Called from the STATUS handler (the demod is a slot), so switching to an analog mode hides the
 * window without losing the user's choice of position or the logs.
 */
export function setDecodeWindowAvailable(demod: string): void {
	const next = (DECODING_MODES as readonly string[]).includes(demod) ? (demod as DecodeMode) : null;
	const changed = next !== mode;
	const leavingDrm = (mode === 'drm' || mode === 'drmplus') && next !== mode;
	mode = next;
	if (leavingDrm) clearDrm();
	const toggle = button('btn-decode-window');
	if (toggle) {
		toggle.disabled = next === null;
		toggle.title = next === null ? t('decode_window_needs_mode') : t('decode_window_tip');
	}
	if (next === null) {
		syncOpen();                    // hide it, but keep the operator's choice (see `wanted`)
		return;
	}
	// Switching between decoding modes re-renders (the other pane) but keeps the window open: the
	// operator asked for the text, not for this particular protocol.
	if (next !== null && changed) syncOpen();     // a decoding mode again: show it if it was wanted
	if (next !== null && toggle && open) toggle.textContent = t('on');
}

/** Wire the window: its buttons, its drag/resize handles and the tune callback. */
export function initDecodeWindow(next: DecodeWindowHooks): void {
	hooks = next;
	const win = element('decode-window');
	const head = element('decode-window-head');
	if (!win) return;

	const geometry = readGeometry();
	// The zoom first, so the geometry applied below is measured against the scale that renders it.
	win.style.zoom = String(getUiScale());
	if (geometry) {
		// A stored position from a bigger window (or a broken one) is pulled back into view: a window
		// the user cannot reach is worse than a moved one.
		applyGeometry(win, clampToViewport(geometry));
	} else {
		// First run: an explicit position inside the viewport. Leaving `left`/`top` unset made the
		// window land wherever its (fixed) static position happened to be - off-screen with its head
		// out of reach, which is a window that cannot be dragged back (reported).
		applyGeometry(win, clampToViewport({
			x: Math.round((window.innerWidth - DEFAULT_SIZE.w) / 2),
			y: Math.max(60, Math.round(window.innerHeight * 0.25)),
			w: DEFAULT_SIZE.w,
			h: DEFAULT_SIZE.h,
		}));
	}
	element('btn-decode-clear')?.addEventListener('click', () => {
		if (mode === 'sstv') {
			// The head Clear empties the whole SSTV store (all modes) — the thumbnails' Delete
			// handles single/multi entries, so the big red switch here is the bulk action.
			if (!window.confirm(t('sstv_confirm_clear_all'))) return;
			deleteSstvAll();
			sstvThumbsShape = '';
			renderContent();
			return;
		}
		if (mode === 'cw') clearCwLog();
		else if (mode === 'drm' || mode === 'drmplus') clearDrm();
		else clearFt8Spots();
		renderContent();
	});
	element('btn-decode-close')?.addEventListener('click', () => setOpen(false));

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
		// A mode click shows that mode's picture set, and (with Auto off) pins the decode mode to it.
		setSstvModeFilter(mode);
		sstvViewMode = mode;
		if (!sstvAuto) {
			sstvPin = mode;
			hooks.onSstvMode(sstvPin);
			try {
				localStorage.setItem(SSTV_MODE_KEY, String(mode));
			} catch { /* storage off: session-only pin */ }
		}
		renderContent();
	});
	element('sstv-auto')?.addEventListener('click', () => setSstvAuto(!sstvAuto));
	element('sstv-select-all')?.addEventListener('click', () => {
		const status = sstvStatus();
		const all = [status.current, ...status.history].filter(Boolean);
		const every = all.every((e) => e!.selected);
		for (const e of all) setSstvSelected(e!.id, !every);
		renderContent();
	});
	element('btn-sstv-delete')?.addEventListener('click', () => deleteSstvSelected());

	// The Auto toggle and the viewed mode survive a reload.
	try {
		sstvAuto = localStorage.getItem(SSTV_AUTO_KEY) !== '0';
		const storedMode = Number(localStorage.getItem(SSTV_MODE_KEY));
		sstvViewMode = Number.isFinite(storedMode) && storedMode >= 0 ? storedMode : null;
		if (!sstvAuto && sstvViewMode !== null) {
			sstvPin = sstvViewMode;
			hooks.onSstvMode(sstvPin);
		}
	} catch { /* storage off: session-only defaults */ }
	setSstvModeFilter(sstvAuto ? null : sstvViewMode);
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

	// Opacity slider. `input` (not `change`) so the window dims while the handle moves.
	win.style.opacity = String(opacity);
	const slider = element('decode-window-opacity') as HTMLInputElement | null;
	if (slider) {
		slider.value = String(Math.round(opacity * 100));
		slider.addEventListener('input', () => {
			const percent = Number(slider.value);
			opacity = Number.isFinite(percent) ? Math.min(1, Math.max(MIN_OPACITY, percent / 100)) : 1;
			win.style.opacity = String(opacity);
			writeGeometry(currentGeometry(win));
		});
	}

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
			if ((event.target as HTMLElement).closest('button, input')) return;   // the head's own controls
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

	mode = null;
	// The DOM this init writes into is new, so the SSTV buttons and the thumbnails must be built again
	// there; the cached shapes would otherwise skip both.
	sstvButtons = [];
	sstvThumbsShape = '';
	renderContent();
	setOpen(false);
}
