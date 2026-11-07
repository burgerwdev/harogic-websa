import { afterEach, describe, expect, it } from 'vitest';
import { frequencyInput, formatFrequencyValue, readFrequencyHz, wireFrequencyInputs } from '../ui/frequencyInput';

afterEach(() => document.body.replaceChildren());
describe('shared frequency input', () => {
  it('uses one-Hz precision in every unit, matching the main SDR panel', () => {
    expect(formatFrequencyValue(101.7, 'MHz')).toBe('101.700000');
    expect(formatFrequencyValue(101.700001, 'MHz')).toBe('101.700001');
    expect(formatFrequencyValue(101700.001, 'kHz')).toBe('101700.001');
    expect(formatFrequencyValue(101700001, 'Hz')).toBe('101700001');
    expect(formatFrequencyValue(0.101700001, 'GHz')).toBe('0.101700001');
    expect(formatFrequencyValue(9.830198803894, 'MHz')).toBe('9.830199');
    expect(formatFrequencyValue(10.38406783656937, 'MHz')).toBe('10.384068');
  });
  it('preserves the underlying tuned value when only unrelated settings change', () => {
    document.body.innerHTML = frequencyInput(9_830_198.803894);
    const input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    expect(input.value).toBe('9.830199');
    expect(readFrequencyHz(document.body)).toBe(9_830_198.803894);
    const stop = wireFrequencyInputs(document.body);
    document.querySelector<HTMLButtonElement>('[data-frequency-unit=Hz]')!.click();
    expect(readFrequencyHz(document.body)).toBe(9_830_198.803894);
    input.value = '9830200';
    expect(readFrequencyHz(document.body)).toBe(9_830_200);
    stop();
  });
  it.each([['Hz', '100000000'], ['MHz', '100']])('honors explicit whole-Hz input over a rounded original in %s', (unit, value) => {
    document.body.innerHTML = frequencyInput(100_000_000.25, 'frequency-unit', unit);
    const stop = wireFrequencyInputs(document.body), input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    expect(readFrequencyHz(document.body)).toBe(100_000_000.25);
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    expect(readFrequencyHz(document.body)).toBe(100_000_000);
    stop();
  });
  it('preserves an unmodified fractional original through every unit', () => {
    document.body.innerHTML = frequencyInput(100_000_000.25);
    const stop = wireFrequencyInputs(document.body);
    for (const unit of ['Hz', 'kHz', 'GHz', 'MHz']) {
      document.querySelector<HTMLButtonElement>(`[data-frequency-unit="${unit}"]`)!.click();
      expect(readFrequencyHz(document.body)).toBe(100_000_000.25);
    }
    stop();
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
  it.each([
    ['Hz', '100011234.25'], ['kHz', '100011.23425'],
    ['MHz', '100.01123425'], ['GHz', '0.10001123425'],
  ])('preserves explicit sub-Hz input on blur in %s', (unit, value) => {
    document.body.innerHTML = frequencyInput(100_010_000, 'frequency-unit', unit);
    const stop = wireFrequencyInputs(document.body), input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    expect(input.value).toBe(value);
    expect(readFrequencyHz(document.body)).toBe(100_011_234.25);
    stop();
  });
  it('keeps a fractional-Hz draft through unit changes before submission', () => {
    document.body.innerHTML = frequencyInput(100_010_000);
    const stop = wireFrequencyInputs(document.body), input = document.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '100.01123425';
    for (const [unit, value] of [['Hz', '100011234.25'], ['kHz', '100011.23425'], ['GHz', '0.10001123425'], ['MHz', '100.01123425']]) {
      document.querySelector<HTMLButtonElement>(`[data-frequency-unit="${unit}"]`)!.click();
      expect(input.value).toBe(value);
      expect(readFrequencyHz(document.body)).toBe(100_011_234.25);
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
