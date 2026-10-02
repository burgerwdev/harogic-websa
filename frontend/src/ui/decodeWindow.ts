// The decode window: one floating panel for what the demodulator decoded.
//
// Two modes share it, because they are the same thing to an operator ("show me the text"):
//
//   * FT8 - a table of decodes, newest first, with the UTC time, SNR, audio offset and frequency.
//     Clicking a row tunes to that signal, the only action an FT8 operator takes on a decode;
//   * CW - the text the Morse decoder heard, line by line, live while the sender is keying.
//
// One toggle opens it: it is enabled while the demodulator is one that decodes (FT8 or CW) and the
// window then shows that mode's pane. The interaction is the same in both:
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
import { formatLatLon, geoFor } from '../sdr/ft8Geo';
import { getLang, onLangChange, t } from '../core/i18n';
import { getUiScale, onUiScaleChange } from '../core/uiScale';

export interface DecodeWindowHooks {
	/** Tune the receiver to an absolute frequency (an FT8 row was clicked). */
	onTune(hz: number): void;
}

/// The demodulators whose output this window shows.
const DECODING_MODES = ['ft8', 'cw'] as const;
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

let hooks: DecodeWindowHooks = { onTune: () => {} };
let open = false;
/// The operator's choice, which survives a mode switch: leaving CW hides the window (there is
/// nothing to show) but does not forget that they wanted it, so coming back shows it again - the
/// first version cleared the choice on the way out and the window stayed hidden for the rest of the
/// session (reported as "CW stops working after switching the demodulator away and back").
let wanted = false;
/** Which pane the window shows: the demodulator decides (null = nothing decodes right now). */
let mode: DecodeMode = null;
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
function rowFor(spot: Ft8Spot): HTMLTableRowElement {
	const row = document.createElement('tr');
	row.className = 'ft8-row';
	row.title = `${t('ft8_tune_tip')}\n${spot.text}\n${clock(spot.at, utcTime)} ${utcTime ? 'UTC' : 'LT'}  ${(spot.hz / 1e6).toFixed(6)} MHz`;
	// The absolute frequency in MHz with 4 decimals is 100 Hz of resolution: enough to place a
	// signal on the waterfall and to tune to it, without the noise of a full-precision number.
	const geo = geoFor(spot.text);
	const isZh = getLang() === 'zh';
	const sender = geo.sender ? (isZh ? geo.sender.zh : geo.sender.name) : '—';
	const country = geo.receiver
		? `${sender} → ${isZh ? geo.receiver.zh : geo.receiver.name}`
		: sender;
	const location = geo.location ? formatLatLon(geo.location) : '—';
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

function renderFt8Rows(): void {
	const body = element('decode-window-rows');
	if (!body) return;
	const spots = ft8Spots();
	body.textContent = '';
	if (spots.length === 0) {
		const row = document.createElement('tr');
		const cell = document.createElement('td');
		cell.colSpan = 7;
		cell.className = 'ft8-empty';
		cell.textContent = t('ft8_waiting');
		row.appendChild(cell);
		body.appendChild(row);
		return;
	}
	for (const spot of spots.slice(0, MAX_ROWS)) {
		body.appendChild(rowFor(spot));
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
	const table = element('decode-window-table');
	const cw = element('decode-window-cw');
	const title = element('decode-window-title');
	const count = element('decode-window-count');
	if (table) table.style.display = showFt8 ? '' : 'none';
	if (cw) cw.style.display = showCw ? '' : 'none';
	const meter = element('cw-meter');
	if (meter) meter.style.display = showCw ? '' : 'none';
	if (title) title.textContent = showCw ? 'CW' : 'FT8';
	const timeTh = element('decode-window-time-th');
	if (timeTh) timeTh.title = utcTime ? 'UTC' : 'LT';
	if (count) {
		count.style.display = showFt8 ? '' : 'none';
		count.textContent = String(ft8Spots().length);
	}
	if (showFt8) renderFt8Rows();
	else if (showCw) renderCwLines();
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
	return () => { offFt8(); offCw(); };
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
	mode = next;
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
		if (mode === 'cw') clearCwLog();
		else clearFt8Spots();
		renderContent();
	});
	element('btn-decode-close')?.addEventListener('click', () => setOpen(false));

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

	// A fresh init knows nothing about the demodulator yet (the STATUS handler calls
	// `setDecodeWindowAvailable` right after): start with no pane rather than rendering whatever
	// mode a previous init left behind (which put an FT8 "waiting" table in the CW session's DOM).
	mode = null;
	renderContent();
	setOpen(false);
}
