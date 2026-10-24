import { expect, it, vi } from 'vitest';
import { buildUnitGroups, unitMap, units } from '../core/units';
import { initKeypad } from '../ui/keypad';

it('commits Filter once in the new unit without clamping to the old unit bounds', () => {
	document.body.innerHTML = `
		<button id="btn-keypad" type="button"></button>
		<input id="input-sdr-ifbw" type="number" min="1" max="20" value="6">
		<div id="unit-sdr_ifbw-group"></div>`;
	localStorage.setItem('websa-keypad', 'on');
	unitMap.set({ ...units(), sdr_ifbw: 'kHz' });
	buildUnitGroups();
	const input = document.getElementById('input-sdr-ifbw') as HTMLInputElement;
	vi.spyOn(input, 'offsetParent', 'get').mockReturnValue(document.body);
	const commits: Array<{ field: string; unit: string; commit: boolean }> = [];
	document.addEventListener('websa:unit-commit',
		(event) => commits.push((event as CustomEvent).detail));
	initKeypad();

	input.dispatchEvent(new MouseEvent('click', { bubbles: true }));
	const key = (label: string) => Array.from(document.querySelectorAll<HTMLButtonElement>('.keypad-key'))
		.find((button) => button.textContent === label)!;
	for (const digit of ['5', '0', '0']) key(digit).click();
	key('Hz').click();

	expect(input.value).toBe('500');
	expect(units().sdr_ifbw).toBe('Hz');
	expect(commits).toEqual([{ field: 'sdr_ifbw', unit: 'Hz', commit: true }]);
	expect(input.dataset.edited).toBeUndefined();

	commits.length = 0;
	input.dispatchEvent(new MouseEvent('click', { bubbles: true }));
	for (const digit of ['0', '.', '1']) key(digit).click();
	key('MHz').click();

	expect(input.value).toBe('0.1');
	expect(units().sdr_ifbw).toBe('MHz');
	expect(commits).toEqual([{ field: 'sdr_ifbw', unit: 'MHz', commit: true }]);
	expect(input.dataset.edited).toBeUndefined();
});
