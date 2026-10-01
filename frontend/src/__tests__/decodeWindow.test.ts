/**
 * The decode window: gated on the demodulator, fed by the FT8 and CW logs, and its rows tune.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { initDecodeWindow, decodeWindowOpen, decodeWindowMode, setDecodeWindowAvailable, toggleDecodeWindow } from '../ui/decodeWindow';
import { addFt8Spot, clearFt8Spots } from '../sdr/ft8Log';
import { addCwText, cwLog } from '../sdr/cwLog';
import { setUiScale } from '../core/uiScale';
import type { Ft8Report } from '../sdr/types';

/** The shell the panel ships (index.html); the window only fills `#decode-window-rows`. */
function shell(): void {
	document.body.innerHTML = `
		<button class="btn" id="btn-decode-window" disabled>Off</button>
		<div class="decode-window" id="decode-window" style="display:none;">
			<div class="decode-window-head" id="decode-window-head">
				<span id="decode-window-title">FT8</span><span class="count" id="decode-window-count">0</span>
				<span class="spacer"></span>
				<input id="decode-window-opacity" type="range" min="35" max="100" step="5" value="100" />
				<button class="btn" id="btn-decode-clear">Clear</button>
				<button class="btn" id="btn-decode-close">Close</button>
			</div>
			<div class="decode-window-body" id="decode-window-body">
				<table id="decode-window-table"><tbody id="decode-window-rows"></tbody></table>
				<div class="decode-cw" id="decode-window-cw" style="display:none;"></div>
			</div>
			<div class="decode-resize-layer">
				<div class="decode-rz n" data-decode-rz="n"></div>
				<div class="decode-rz s" data-decode-rz="s"></div>
				<div class="decode-rz w" data-decode-rz="w"></div>
				<div class="decode-rz e" data-decode-rz="e"></div>
				<div class="decode-rz nw" data-decode-rz="nw"></div>
				<div class="decode-rz ne" data-decode-rz="ne"></div>
				<div class="decode-rz sw" data-decode-rz="sw"></div>
				<div class="decode-rz se" data-decode-rz="se"></div>
			</div>
		</div>`;
}

const report = (over: Partial<Ft8Report> = {}): Ft8Report => ({
	text: 'CQ JO1WKO PM95',
	frequencyHz: 1000,
	timeOffsetS: 0.02,
	snrDb: 12,
	count: 1,
	centerHz: 21_074_000,
	...over,
});

const rows = () => Array.from(document.querySelectorAll('#decode-window-rows tr'));
const shown = () => (document.getElementById('decode-window') as HTMLElement).style.display;

describe('the FT8 decode window', () => {
	beforeEach(() => {
		shell();
		clearFt8Spots();
		localStorage.clear();
		initDecodeWindow({ onTune: () => {} });
	});

	it('is disabled until FT8 is the demodulator, and hides when it stops being', () => {
		const button = document.getElementById('btn-decode-window') as HTMLButtonElement;
		expect(button.disabled).toBe(true);
		setDecodeWindowAvailable('ft8');
		expect(button.disabled).toBe(false);
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		expect(shown()).toBe('flex');
		// Switching to an analog demodulator closes it (the table would be meaningless) but the log
		// and the user's chosen position stay.
		setDecodeWindowAvailable('am');
		expect(decodeWindowOpen()).toBe(false);
		expect(shown()).toBe('none');
		expect(button.disabled).toBe(true);
	});

	it('shows a waiting row, then one row per decode, newest first', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		expect(document.querySelector('.ft8-empty')).not.toBeNull();

		addFt8Spot(report({ text: 'CQ JO1WKO PM95', frequencyHz: 1000 }), Date.UTC(2026, 8, 21, 1, 2, 3));
		addFt8Spot(report({ text: 'K1ABC W9XYZ EN37', frequencyHz: 1800, snrDb: -5 }), Date.UTC(2026, 8, 21, 1, 2, 18));
		const table = rows();
		expect(table.length).toBe(2);
		const first = Array.from(table[0].querySelectorAll('td')).map((cell) => cell.textContent);
		expect(first[0]).toBe('01:02:18');
		expect(first[1]).toBe('-5');
		expect(first[2]).toBe('1800');
		expect(first[3]).toBe('21.0758');
		expect(first[4]).toBe('K1ABC W9XYZ EN37');
		expect(document.getElementById('decode-window-count')?.textContent).toBe('2');
	});

	it('tunes to the signal a row was clicked on', () => {
		const onTune = vi.fn();
		initDecodeWindow({ onTune });
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report({ frequencyHz: 1450 }), 1_000);
		rows()[0].dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onTune).toHaveBeenCalledWith(21_075_450);
	});

	it('clears the table from its own button', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report(), 1_000);
		expect(rows().length).toBe(1);
		(document.getElementById('btn-decode-clear') as HTMLButtonElement).click();
		expect(document.querySelector('.ft8-empty')).not.toBeNull();
	});

	it('remembers where and how big the operator left it', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const head = document.getElementById('decode-window-head') as HTMLElement;
		head.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointermove', { clientX: 160, clientY: 140, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointerup', { clientX: 160, clientY: 140, bubbles: true }));
		expect(win.style.left).not.toBe('');
		const stored = JSON.parse(localStorage.getItem('websa-decode-window') || '{}');
		expect(typeof stored.x).toBe('number');
		expect(stored.w).toBeGreaterThan(0);
	});

	it('dims the window from its head, remembers the level, and stays grabbable', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const slider = document.getElementById('decode-window-opacity') as HTMLInputElement;
		expect(win.style.opacity).toBe('1');

		slider.value = '60';
		slider.dispatchEvent(new Event('input', { bubbles: true }));
		expect(win.style.opacity).toBe('0.6');
		expect(JSON.parse(localStorage.getItem('websa-decode-window') || '{}').opacity).toBeCloseTo(0.6);

		// The floor is a guard, not a preference: an invisible window cannot be dragged back, which is
		// the same reason the position is clamped into the viewport instead of restored blindly.
		slider.value = '0';
		slider.dispatchEvent(new Event('input', { bubbles: true }));
		expect(win.style.opacity).toBe('0.35');

		// The slider is a control in the drag handle: grabbing it must move the handle, not the window.
		const before = win.style.left;
		slider.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		(document.getElementById('decode-window-head') as HTMLElement)
			.dispatchEvent(new PointerEvent('pointermove', { clientX: 260, clientY: 220, bubbles: true }));
		expect(win.style.left).toBe(before);
	});

	it('drags in viewport pixels while carrying the UI scale (the zoom fix)', () => {		setUiScale(2);   // jsdom lays nothing out, so this pins the conversion convention only
		try {
			setDecodeWindowAvailable('ft8');
			const win = document.getElementById('decode-window') as HTMLElement;
			const head = document.getElementById('decode-window-head') as HTMLElement;
			expect(win.style.zoom).toBe('2');

			// Grab the head at the cursor and drag: the styles store local px (half the viewport
			// distance at scale 2), while the persisted geometry stays in viewport px - that split is
			// what keeps the cursor anchored to the head on a scaled UI.
			head.dispatchEvent(new PointerEvent('pointerdown', { clientX: 300, clientY: 200, bubbles: true }));
			head.dispatchEvent(new PointerEvent('pointermove', { clientX: 500, clientY: 300, bubbles: true }));
			head.dispatchEvent(new PointerEvent('pointerup', { clientX: 500, clientY: 300, bubbles: true }));
			// jsdom reports a 0x0 rect, so the grab offset is the full cursor position (300, 200) and
			// the move lands the window's origin at (200, 100) viewport px.
			expect(win.style.left).toBe('100px');   // 200 viewport px / scale 2
			expect(win.style.top).toBe('50px');
			const stored = JSON.parse(localStorage.getItem('websa-decode-window') || '{}');
			expect(stored.x).toBe(200);             // persisted in viewport px
			expect(stored.y).toBe(100);

			// A later init at the same scale restores the same spot: the stored viewport px go back
			// through the same division, not in as raw style values.
			initDecodeWindow({ onTune: () => {} });
			expect(win.style.left).toBe('100px');
			expect(win.style.top).toBe('50px');
		} finally {
			setUiScale(1);
		}
	});

	it('resizes from all four edges and four corners', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const box = (el: string) => document.querySelector(`[data-decode-rz="${el}"]`) as HTMLElement;
		const drag = (el: string, dx: number, dy: number) => {
			box(el).dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
			box(el).dispatchEvent(new PointerEvent('pointermove', { clientX: 100 + dx, clientY: 100 + dy, bubbles: true }));
			box(el).dispatchEvent(new PointerEvent('pointerup', { clientX: 100 + dx, clientY: 100 + dy, bubbles: true }));
		};
		// jsdom has no layout: geometry starts from the inline styles the init wrote (the scale-1
		// defaults) and each drag is a delta on the captured start box.
		const w0 = Number.parseFloat(win.style.width);
		const h0 = Number.parseFloat(win.style.height);
		expect(box('n') && box('s') && box('e') && box('w')).toBeTruthy();
		drag('e', 40, 0);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 40);
		drag('s', 0, 30);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 30);
		// The west edge moves the origin with it; the height is untouched by a horizontal drag.
		const left0 = Number.parseFloat(win.style.left);
		drag('w', -20, 0);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 60);
		expect(Number.parseFloat(win.style.left)).toBe(left0 - 20);
		// The north edge moves the top and shrinks the box from that side.
		const top0 = Number.parseFloat(win.style.top);
		drag('n', 0, -10);
		expect(Number.parseFloat(win.style.top)).toBe(top0 - 10);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 40);
		// A corner changes both axes at once.
		drag('se', 25, 15);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 85);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 55);
	});

	it('keeps the opposite edge fixed when a resize hits the minimum width', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const east = document.querySelector('[data-decode-rz="e"]') as HTMLElement;
		const left0 = Number.parseFloat(win.style.left);
		// Pull the west edge far past the 260 px floor: the east edge (left0 + width) must not move.
		const right0 = left0 + Number.parseFloat(win.style.width);
		const west = document.querySelector('[data-decode-rz="w"]') as HTMLElement;
		west.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		west.dispatchEvent(new PointerEvent('pointermove', { clientX: 3000, clientY: 100, bubbles: true }));
		west.dispatchEvent(new PointerEvent('pointerup', { clientX: 3000, clientY: 100, bubbles: true }));
		expect(Number.parseFloat(win.style.width)).toBe(260);
		expect(Number.parseFloat(win.style.left) + 260).toBe(right0);
		expect(east).toBeTruthy();
	});

	it('shows the CW text when CW is the demodulator, and clears it from the same button', () => {
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		expect(decodeWindowMode()).toBe('cw');
		expect(document.getElementById('decode-window-title')?.textContent).toBe('CW');
		expect((document.getElementById('decode-window-cw') as HTMLElement).style.display).toBe('');
		expect((document.getElementById('decode-window-table') as HTMLElement).style.display).toBe('none');

		addCwText('CQ DE', false);
		addCwText(' N0CALL', true);
		const pane = document.getElementById('decode-window-cw') as HTMLElement;
		expect(pane.textContent).toContain('CQ DE N0CALL');
		expect(cwLog().lines.length).toBe(1);

		(document.getElementById('btn-decode-clear') as HTMLButtonElement).click();
		expect(cwLog().lines.length).toBe(0);
		// Clearing the CW pane must not touch the FT8 log behind it.
		addFt8Spot(report(), 1000);
		expect(document.querySelectorAll('#decode-window-rows tr').length).toBe(0);
	});

	it('keeps the window open when the operator switches between decoding modes', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		setDecodeWindowAvailable('cw');
		expect(decodeWindowOpen()).toBe(true);
		expect(decodeWindowMode()).toBe('cw');
		expect((document.getElementById('decode-window-title') as HTMLElement).textContent).toBe('CW');
	});

	it('is closed and disabled for a mode that does not decode', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		setDecodeWindowAvailable('am');
		expect(decodeWindowOpen()).toBe(false);
		expect(decodeWindowMode()).toBe(null);
		expect((document.getElementById('btn-decode-window') as HTMLButtonElement).disabled).toBe(true);
	});

	it('comes back when a decoding mode returns (the choice is not forgotten)', () => {
		// Reported: after leaving CW for another demodulator and coming back, the window stayed
		// hidden - the first version cleared the operator's choice on the way out - so the decode
		// list looked dead ("CW stops working after switching the demodulator away and back").
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		setDecodeWindowAvailable('usb');
		expect(decodeWindowOpen()).toBe(false);
		setDecodeWindowAvailable('cw');
		expect(decodeWindowOpen()).toBe(true);
		expect((document.getElementById('decode-window') as HTMLElement).style.display).toBe('flex');
	});
});
