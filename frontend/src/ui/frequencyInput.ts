import { convertUnitValue } from '../core/units';
import { getLang } from '../core/i18n';

const frequencyUnits = ['Hz', 'kHz', 'MHz', 'GHz'] as const;

/** Keep a whole-Hz precision in every unit, and retain fractional Hz when supplied. */
export function formatFrequencyValue(value: number, unit: string): string {
  if (!Number.isFinite(value)) return '';
  const digits = ({ Hz: 0, kHz: 3, MHz: 6, GHz: 9 } as Record<string, number>)[unit] ?? 0;
  const rounded = Number(value.toPrecision(15));
  const [mantissa, exponent = '0'] = String(rounded).split('e');
  const fraction = Math.max(0, (mantissa.split('.')[1]?.length ?? 0) - Number(exponent));
  return rounded.toFixed(Math.max(digits, Math.min(12, fraction)));
}

/** Shared unit toggles use the same unit-btn/active convention as the main instrument. */
export function frequencyInput(hz: number, unitName = 'frequency-unit', unit = 'MHz'): string {
  const label = getLang() === 'zh' ? '频率单位' : 'Frequency unit';
  return `<span class="mc-frequency-input" data-frequency-input data-unit="${unit}"><input name="frequency" data-frequency-value type="number" min="0" step="any" value="${formatFrequencyValue(convertUnitValue(hz, 'Hz', unit), unit)}" required><input type="hidden" name="${unitName}" value="${unit}" data-frequency-unit-value><span class="unit-btn-group" role="group" aria-label="${label}">${frequencyUnits.map(value => `<button type="button" class="unit-btn${value === unit ? ' active' : ''}" data-frequency-unit="${value}" aria-pressed="${value === unit}" title="${label} · ${value}">${value}</button>`).join('')}</span></span>`;
}

/** Synchronize markup after restoring a draft without converting its already-restored value. */
export function syncFrequencyInputs(host: HTMLElement): void {
  host.querySelectorAll<HTMLElement>('[data-frequency-input]').forEach(group => {
    const unit = group.querySelector<HTMLInputElement>('[data-frequency-unit-value]')!.value;
    group.dataset.unit = unit;
    group.querySelectorAll<HTMLButtonElement>('[data-frequency-unit]').forEach(button => {
      const active = button.dataset.frequencyUnit === unit;
      button.classList.toggle('active', active); button.setAttribute('aria-pressed', String(active));
    });
  });
}

export function wireFrequencyInputs(host: HTMLElement): () => void {
  const click = (event: Event): void => {
    const button = (event.target as Element).closest<HTMLButtonElement>('[data-frequency-unit]');
    if (!button || button.disabled) return;
    event.preventDefault();
    const group = button.closest<HTMLElement>('[data-frequency-input]')!;
    const unit = group.querySelector<HTMLInputElement>('[data-frequency-unit-value]')!;
    if (unit.value === button.dataset.frequencyUnit) return;
    unit.value = button.dataset.frequencyUnit!;
    unit.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const change = (event: Event): void => {
    const unit = event.target as HTMLInputElement;
    if (!unit.matches('[data-frequency-unit-value]')) return;
    const group = unit.closest<HTMLElement>('[data-frequency-input]')!;
    const input = group.querySelector<HTMLInputElement>('[data-frequency-value]')!;
    if (input.value.trim()) input.value = formatFrequencyValue(convertUnitValue(Number(input.value), group.dataset.unit!, unit.value), unit.value);
    syncFrequencyInputs(group.parentElement!);
  };
  const blur = (event: Event): void => {
    const input = event.target as HTMLInputElement;
    if (!input.matches('[data-frequency-value]') || !input.value.trim() || !input.validity.valid) return;
    input.value = formatFrequencyValue(Number(input.value), input.closest<HTMLElement>('[data-frequency-input]')!.dataset.unit!);
  };
  host.addEventListener('click', click); host.addEventListener('change', change); host.addEventListener('focusout', blur);
  return () => { host.removeEventListener('click', click); host.removeEventListener('change', change); host.removeEventListener('focusout', blur); };
}
