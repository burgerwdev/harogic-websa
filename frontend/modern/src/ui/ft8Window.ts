// The FT8 decode window: a floating table of what the decoder has heard.
//
// The panel's FT8 line shows only the newest transmission, which is enough while a band is quiet and
// useless while it is busy. Open-source FT8 clients (WSJT-X, JTDX, GridTracker) all grew a decodes
// list for the same reason, and the interaction that makes it useful is clicking a row: the operator
// reads a callsign, clicks it, and is listening to it. So this window is:
//
//   * opened from a toggle next to the FT8 readout, and only while FT8 is the demodulator;
//   * draggable by its head and resizable by its corner (position and size persist, like the keypad);
//   * newest first, so a new decode is visible without scrolling, with the UTC time, the SNR, the
//     audio offset (`DF`) and the absolute frequency;
//   * click-to-tune: a row tunes the receiver to that signal (`onTune`), which is the only action an
//     FT8 operator performs on a decode;
//   * capped, with a Clear button, so a long session cannot grow without bound.
import { clearFt8Spots, ft8Spots, subscribeFt8Spots, utcClock, type Ft8Spot } from '../sdr/ft8Log';
import { t } from '../core/i18n';

export interface Ft8WindowHooks {
	/** Tune the receiver to an absolute frequency (a row was clicked). */
	onTune(hz: number): void;
}

const LS_KEY = 'websa-ft8-window';
const DEFAULT_SIZE = { w: 460, h: 240 };
/// The floor keeps the window grabbable. An invisible would be worse than a moved one: the same
/// reason the position is clamped into the viewport rather than restored blindly.
const MIN_OPACITY = 0.35;
/// Live opacity, persisted alongside the geometry (one record, one writer).
let opacity = 1;
/** Rows rendered; the log holds more, but a table nobody scrolls should not cost layout time. */
const MAX_ROWS = 100;

let hooks: Ft8WindowHooks = { onTune: () => {} };
let open = false;
/** True while FT8 is the demodulator: the toggle is disabled (and the window hidden) otherwise. */
let available = false;
let unsubscribe: (() => void) | null = null;

interface Geometry {
	x: number;
	y: number;
	w: number;
	h: number;
}

function readGeometry(): Geometry | null {
	try {
		const raw = localStorage.getItem(LS_KEY);
		if (!raw) return null;
		const parsed = JSON.parse(raw) as Partial<Geometry> & { opacity?: number };
		if (typeof parsed.x !== 'number' || typeof parsed.y !== 'number') return null;
		const stored = Number(parsed.opacity);
		opacity = Number.isFinite(stored) ? Math.min(1, Math.max(MIN_OPACITY, stored)) : 1;
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
		// The opacity rides along: one record, one writer, so the two can never drift apart.
		localStorage.setItem(LS_KEY, JSON.stringify({ ...geometry, opacity }));
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

/**
 * The window's geometry as the DOM sees it.
 *
 * `getBoundingClientRect` is the truth in a browser, but a layout-less environment (jsdom in the unit
 * tests, a hidden window after a re-init) reports zeros - and persisting a 0x0 window would then
 * restore an unusable one, so the inline style and the defaults are the fallbacks.
 */
function currentGeometry(win: HTMLElement): Geometry {
	const box = win.getBoundingClientRect();
	const styled = (value: string) => Math.max(0, Math.round(Number.parseFloat(value) || 0));
	return {
		x: box.width > 0 ? box.left : styled(win.style.left),
		y: box.height > 0 ? box.top : styled(win.style.top),
		w: Math.max(260, Math.round(box.width) || win.offsetWidth || styled(win.style.width) || DEFAULT_SIZE.w),
		h: Math.max(120, Math.round(box.height) || win.offsetHeight || styled(win.style.height) || DEFAULT_SIZE.h),
	};
}
const button = (id: string) => document.getElementById(id) as HTMLButtonElement | null;

function renderRows(): void {
	const body = element('ft8-window-rows');
	if (!body) return;
	const spots = ft8Spots();
	const count = element('ft8-window-count');
	if (count) count.textContent = String(spots.length);
	body.textContent = '';
	if (spots.length === 0) {
		const row = document.createElement('tr');
		const cell = document.createElement('td');
		cell.colSpan = 5;
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

function rowFor(spot: Ft8Spot): HTMLTableRowElement {
	const row = document.createElement('tr');
	row.className = 'ft8-row';
	row.title = `${t('ft8_tune_tip')}\n${spot.text}\n${utcClock(spot.at)} UTC  ${(spot.hz / 1e6).toFixed(6)} MHz`;
	// The absolute frequency in MHz with 4 decimals is 100 Hz of resolution: enough to place a
	// signal on the waterfall and to tune to it, without the noise of a full-precision number.
	const cells = [
		utcClock(spot.at),
		spot.snrDb.toFixed(0),
		spot.offsetHz.toFixed(0),
		(spot.hz / 1e6).toFixed(4),
		spot.text,
	];
	for (const value of cells) {
		const cell = document.createElement('td');
		cell.textContent = value;
		row.appendChild(cell);
	}
	row.addEventListener('click', () => hooks.onTune(spot.hz));
	return row;
}

/** Show/hide the window (the toggle). */
function setOpen(next: boolean): void {
	open = next && available;
	const win = element('ft8-window');
	const toggle = button('btn-ft8-window');
	if (win) win.style.display = open ? 'flex' : 'none';
	if (toggle) {
		toggle.textContent = open ? t('on') : t('off');
		toggle.classList.toggle('active', open);
	}
	if (open) {
		renderRows();
		if (!unsubscribe) unsubscribe = subscribeFt8Spots(renderRows);
	} else if (unsubscribe) {
		unsubscribe();
		unsubscribe = null;
	}
}

export function toggleFt8Window(): void {
	setOpen(!open);
}

/** True while the window is showing (tests and the button state). */
export function ft8WindowOpen(): boolean {
	return open;
}

/**
 * Gate the control on the demodulator: FT8's toggle only means something when FT8 is selected.
 *
 * Called from the STATUS handler (the demod is a slot), so switching away hides the window without
 * losing the user's choice of position or their log.
 */
export function setFt8WindowAvailable(next: boolean): void {
	available = next;
	const toggle = button('btn-ft8-window');
	if (toggle) {
		toggle.disabled = !next;
		toggle.title = next ? t('ft8_window_tip') : t('ft8_window_needs_ft8');
	}
	if (!next && open) setOpen(false);
	if (next && toggle && open) toggle.textContent = t('on');
}

/** Wire the window: its buttons, its drag/resize handles and the tune callback. */
export function initFt8Window(next: Ft8WindowHooks): void {
	hooks = next;
	const win = element('ft8-window');
	const head = element('ft8-window-head');
	const handle = element('ft8-window-resize');
	if (!win) return;

	const geometry = readGeometry();
	if (geometry) {
		// A stored position from a bigger window (or a broken one) is pulled back into view: a window
		// the user cannot reach is worse than a moved one.
		const clamped = clampToViewport(geometry);
		win.style.left = `${clamped.x}px`;
		win.style.top = `${clamped.y}px`;
		win.style.width = `${clamped.w}px`;
		win.style.height = `${clamped.h}px`;
	} else {
		// First run: an explicit position inside the viewport. Leaving `left`/`top` unset made the
		// window land wherever its (fixed) static position happened to be - off-screen with its head
		// out of reach, which is a window that cannot be dragged back (reported).
		const start = clampToViewport({
			x: Math.round((window.innerWidth - DEFAULT_SIZE.w) / 2),
			y: Math.max(60, Math.round(window.innerHeight * 0.25)),
			w: DEFAULT_SIZE.w,
			h: DEFAULT_SIZE.h,
		});
		win.style.left = `${start.x}px`;
		win.style.top = `${start.y}px`;
		win.style.width = `${DEFAULT_SIZE.w}px`;
		win.style.height = `${DEFAULT_SIZE.h}px`;
	}
	element('btn-ft8-clear')?.addEventListener('click', () => clearFt8Spots());
	element('btn-ft8-close')?.addEventListener('click', () => setOpen(false));

	// Opacity slider. `input` (not `change`) so the window dims while the handle moves.
	win.style.opacity = String(opacity);
	const slider = element('ft8-window-opacity') as HTMLInputElement | null;
	if (slider) {
		slider.value = String(Math.round(opacity * 100));
		slider.addEventListener('input', () => {
			const percent = Number(slider.value);
			opacity = Number.isFinite(percent) ? Math.min(1, Math.max(MIN_OPACITY, percent / 100)) : 1;
			win.style.opacity = String(opacity);
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
			const box = currentGeometry(win);
			const next = clampToViewport({
				x: event.clientX - offsetX,
				y: event.clientY - offsetY,
				w: box.w,
				h: box.h,
			});
			win.style.left = `${next.x}px`;
			win.style.top = `${next.y}px`;
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

	if (handle) {
		let resizing = false;
		let startX = 0;
		let startY = 0;
		let startW = 0;
		let startH = 0;
		handle.addEventListener('pointerdown', (event) => {
			const box = currentGeometry(win);
			resizing = true;
			startX = event.clientX;
			startY = event.clientY;
			startW = box.w;
			startH = box.h;
			try {
				handle.setPointerCapture(event.pointerId);
			} catch {
				/* not capturable */
			}
			event.preventDefault();
		});
		handle.addEventListener('pointermove', (event) => {
			if (!resizing) return;
			// The window grows down-right from its own top-left corner: the drag is a delta on the
			// size, and the table's flex layout does the rest.
			win.style.width = `${Math.max(260, startW + (event.clientX - startX))}px`;
			win.style.height = `${Math.max(120, startH + (event.clientY - startY))}px`;
		});
		const stop = (event: PointerEvent) => {
			if (!resizing) return;
			resizing = false;
			writeGeometry(currentGeometry(win));
			try {
				handle.releasePointerCapture(event.pointerId);
			} catch {
				/* never captured */
			}
		};
		handle.addEventListener('pointerup', stop);
		handle.addEventListener('pointercancel', stop);
	}

	renderRows();
	setOpen(false);
}
