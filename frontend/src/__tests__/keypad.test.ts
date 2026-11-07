import { afterEach, expect, it, vi } from 'vitest';
import { buildUnitGroups, unitMap, units } from '../core/units';
import { initKeypad, isKeypadOpen } from '../ui/keypad';

afterEach(() => {
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  document.body.replaceChildren();
  vi.restoreAllMocks();
  localStorage.clear();
});

it('initializes the keypad when its optional saved preference is unreadable', () => {
  document.body.innerHTML = '<button id="btn-keypad" type="button"></button>';
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new DOMException('Blocked', 'SecurityError'); });
  expect(() => initKeypad()).not.toThrow();
  expect(document.getElementById('btn-keypad')!.getAttribute('aria-pressed')).toBe('false');
});

it('applies keypad toggles even when preference persistence is unavailable', () => {
  document.body.innerHTML = '<button id="btn-keypad" type="button"></button>';
  initKeypad();
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('Full', 'QuotaExceededError'); });
  const errors: unknown[] = [];
  const onError = (event: ErrorEvent) => { errors.push(event.error); event.preventDefault(); };
  window.addEventListener('error', onError);
  try {
    document.getElementById('btn-keypad')!.click();
    expect(document.getElementById('btn-keypad')!.getAttribute('aria-pressed')).toBe('true');
    expect(errors).toEqual([]);
  } finally { window.removeEventListener('error', onError); }
});

it.each(['disabled', 'readOnly', 'detached'])('cancels a keypad draft when its target becomes %s', state => {
  document.body.innerHTML = '<button id="btn-keypad" type="button"></button><input id="plain-number" type="number" value="6">';
  localStorage.setItem('websa-keypad', 'on');
  initKeypad();
  const input = document.querySelector<HTMLInputElement>('#plain-number')!;
  vi.spyOn(input, 'offsetParent', 'get').mockReturnValue(document.body);
  const change = vi.fn();
  input.addEventListener('change', change);
  input.click();
  expect(isKeypadOpen()).toBe(true);
  const key = (label: string) => [...document.querySelectorAll<HTMLButtonElement>('.keypad-key')]
    .find(button => button.textContent === label)!;
  key('9').click();
  if (state === 'disabled') input.disabled = true;
  else if (state === 'readOnly') input.readOnly = true;
  else input.remove();
  key('OK').click();
  expect(input.value).toBe('6');
  expect(change).not.toHaveBeenCalled();
  expect(isKeypadOpen()).toBe(false);
});

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
	expect(isKeypadOpen()).toBe(true);
	const key = (label: string) => Array.from(document.querySelectorAll<HTMLButtonElement>('.keypad-key'))
		.find((button) => button.textContent === label)!;
	for (const digit of ['5', '0', '0']) key(digit).click();
	key('Hz').click();
	expect(isKeypadOpen()).toBe(false);

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
