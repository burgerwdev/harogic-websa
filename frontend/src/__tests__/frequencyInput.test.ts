import { afterEach, describe, expect, it } from 'vitest';
import { frequencyInput, formatFrequencyValue, wireFrequencyInputs } from '../ui/frequencyInput';

afterEach(() => document.body.replaceChildren());
describe('shared frequency input', () => {
  it('uses stable unit precision without losing one-Hz or fractional-Hz tuning', () => {
    expect(formatFrequencyValue(101.7, 'MHz')).toBe('101.700000');
    expect(formatFrequencyValue(101.700001, 'MHz')).toBe('101.700001');
    expect(formatFrequencyValue(101700.001, 'kHz')).toBe('101700.001');
    expect(formatFrequencyValue(101700001, 'Hz')).toBe('101700001');
    expect(formatFrequencyValue(0.101700001, 'GHz')).toBe('0.101700001');
    expect(formatFrequencyValue(101.7000005, 'MHz')).toBe('101.7000005');
  });
  it('converts the current draft through unit toggles without applying it', () => {
    document.body.innerHTML = frequencyInput(101700001);
    const stop = wireFrequencyInputs(document.body), input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '101.700002';
    for (const [unit, expected] of [['Hz', '101700002'], ['kHz', '101700.002'], ['GHz', '0.101700002'], ['MHz', '101.700002']]) {
      document.querySelector<HTMLButtonElement>(`[data-frequency-unit="${unit}"]`)!.click();
      expect(input.value).toBe(expected);
      expect(document.querySelector('[aria-pressed=true]')!.textContent).toBe(unit);
      expect(document.querySelector<HTMLInputElement>('[name=frequency-unit]')!.value).toBe(unit);
    }
    stop();
  });
  it('leaves an empty draft empty when toggling and normalizes only after editing', () => {
    document.body.innerHTML = frequencyInput(7100000);
    const stop = wireFrequencyInputs(document.body), input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '';
    document.querySelector<HTMLButtonElement>('[data-frequency-unit=kHz]')!.click();
    expect(input.value).toBe('');
    input.value = '7100.1'; input.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    expect(input.value).toBe('7100.100');
    input.value = '0.001';
    document.querySelector<HTMLButtonElement>('[data-frequency-unit=GHz]')!.click();
    expect(input.value).toBe('0.000000001'); expect(input.validity.valid).toBe(true);
    stop();
  });
});
