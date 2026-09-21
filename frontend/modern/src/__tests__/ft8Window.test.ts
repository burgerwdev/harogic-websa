/**
 * The FT8 decode window: gated on the demodulator, fed by the log, and its rows tune.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { initFt8Window, ft8WindowOpen, setFt8WindowAvailable, toggleFt8Window } from '../ui/ft8Window';
import { addFt8Spot, clearFt8Spots } from '../sdr/ft8Log';
import type { Ft8Report } from '../sdr/types';

/** The shell the panel ships (index.html); the window only fills `#ft8-window-rows`. */
function shell(): void {
	document.body.innerHTML = `
		<button class="btn" id="btn-ft8-window" disabled>Off</button>
		<div class="ft8-window" id="ft8-window" style="display:none;">
			<div class="ft8-window-head" id="ft8-window-head">
				<span>FT8</span><span class="count" id="ft8-window-count">0</span>
				<span class="spacer"></span>
				<button class="btn" id="btn-ft8-clear">Clear</button>
				<button class="btn" id="btn-ft8-close">Close</button>
			</div>
			<div class="ft8-window-body"><table><tbody id="ft8-window-rows"></tbody></table></div>
			<div class="ft8-resize" id="ft8-window-resize"></div>
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

const rows = () => Array.from(document.querySelectorAll('#ft8-window-rows tr'));
const shown = () => (document.getElementById('ft8-window') as HTMLElement).style.display;

describe('the FT8 decode window', () => {
	beforeEach(() => {
		shell();
		clearFt8Spots();
		localStorage.clear();
		initFt8Window({ onTune: () => {} });
	});

	it('is disabled until FT8 is the demodulator, and hides when it stops being', () => {
		const button = document.getElementById('btn-ft8-window') as HTMLButtonElement;
		expect(button.disabled).toBe(true);
		setFt8WindowAvailable(true);
		expect(button.disabled).toBe(false);
		toggleFt8Window();
		expect(ft8WindowOpen()).toBe(true);
		expect(shown()).toBe('flex');
		// Switching to an analog demodulator closes it (the table would be meaningless) but the log
		// and the user's chosen position stay.
		setFt8WindowAvailable(false);
		expect(ft8WindowOpen()).toBe(false);
		expect(shown()).toBe('none');
		expect(button.disabled).toBe(true);
	});

	it('shows a waiting row, then one row per decode, newest first', () => {
		setFt8WindowAvailable(true);
		toggleFt8Window();
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
		expect(document.getElementById('ft8-window-count')?.textContent).toBe('2');
	});

	it('tunes to the signal a row was clicked on', () => {
		const onTune = vi.fn();
		initFt8Window({ onTune });
		setFt8WindowAvailable(true);
		toggleFt8Window();
		addFt8Spot(report({ frequencyHz: 1450 }), 1_000);
		rows()[0].dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onTune).toHaveBeenCalledWith(21_075_450);
	});

	it('clears the table from its own button', () => {
		setFt8WindowAvailable(true);
		toggleFt8Window();
		addFt8Spot(report(), 1_000);
		expect(rows().length).toBe(1);
		(document.getElementById('btn-ft8-clear') as HTMLButtonElement).click();
		expect(document.querySelector('.ft8-empty')).not.toBeNull();
	});

	it('remembers where and how big the operator left it', () => {
		setFt8WindowAvailable(true);
		const win = document.getElementById('ft8-window') as HTMLElement;
		const head = document.getElementById('ft8-window-head') as HTMLElement;
		head.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointermove', { clientX: 160, clientY: 140, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointerup', { clientX: 160, clientY: 140, bubbles: true }));
		expect(win.style.left).not.toBe('');
		const stored = JSON.parse(localStorage.getItem('websa-ft8-window') || '{}');
		expect(typeof stored.x).toBe('number');
		expect(stored.w).toBeGreaterThan(0);
	});
});
