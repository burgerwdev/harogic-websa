// Reference-clock: minimal detail popover (current source + output) + box flash on apply.
import { t } from './i18n';

export type RefClockStatus = 'applied' | 'fallback' | 'forced' | 'unverified';

const SOURCE_KEYS: Record<number, string> = {
  0: 'internal',
  1: 'external',
  2: 'premium',
  3: 'external_forced',
};

export function refClockSourceName(src: number | null | undefined): string | null {
  if (src == null || !isFinite(src)) return null;
  return SOURCE_KEYS[Math.round(src)] ?? null;
}

/** Compare the requested source with the source the device reports (used for the flash colour). */
export function refClockStatus(
  requested: string,
  actualSrc: number | null | undefined,
): RefClockStatus {
  const actual = refClockSourceName(actualSrc);
  if (actual === null) return 'unverified';
  if (requested === 'external_forced') return 'forced';
  if (requested === 'external' && actual === 'internal') return 'fallback';
  if (actual === requested) return 'applied';
  return 'unverified';
}

function currentSelectionLabel(): string {
  const sel = document.getElementById('select-refclk') as HTMLSelectElement | null;
  if (!sel) return '—';
  const option = sel.selectedOptions && sel.selectedOptions[0];
  return (option?.textContent || sel.value || '—').trim();
}

export function fillRefClockDetail(): void {
  const el = document.getElementById('refclk-d-current');
  if (el) el.textContent = currentSelectionLabel();
}

let flashTimer: number | null = null;

/** Subtle 3x outline flash on the Clock Ref box to confirm the switch was applied. */
export function flashRefClockBox(status?: RefClockStatus): void {
  const sel = document.getElementById('select-refclk');
  const box = sel?.closest('.info-item') as HTMLElement | null;
  if (!box) return;
  box.classList.remove('refclk-flash', 'warn');
  void box.offsetWidth;   // restart the animation on consecutive switches
  box.classList.add('refclk-flash');
  if (status === 'fallback' || status === 'forced') box.classList.add('warn');
  if (flashTimer !== null) window.clearTimeout(flashTimer);
  flashTimer = window.setTimeout(() => {
    box.classList.remove('refclk-flash', 'warn');
    flashTimer = null;
  }, 1500);
}

/** Called from STATUS handling: flash on the command response, refresh the popover if open. */
export function refreshRefClockHint(status: any, responseTo?: string): void {
  if (responseTo === 'SET_REFCK' || responseTo === 'SET_REFCKOUT') {
    const actual = status?.mode === 'rta' ? status?.rta_actual : status?.actual;
    flashRefClockBox(refClockStatus(String(status?.ref_clock ?? 'internal'),
      actual?.refclk_src as number | null));
  }
  const pop = document.getElementById('refclk-popover');
  if (pop && pop.style.display !== 'none') fillRefClockDetail();
}

export function openRefClockDetail(): void {
  fillRefClockDetail();
  const pop = document.getElementById('refclk-popover');
  if (pop) pop.style.display = '';
}

export function closeRefClockDetail(): void {
  const pop = document.getElementById('refclk-popover');
  if (pop) pop.style.display = 'none';
}

// Kept for the i18n/status helpers used elsewhere.
export const REFCLK_STATUS_TEXT: Record<RefClockStatus, string> = {
  applied: 'refclk_applied',
  fallback: 'refclk_fallback',
  forced: 'refclk_forced',
  unverified: 'refclk_unverified',
};

export function refClockStatusText(status: RefClockStatus): string {
  return t(REFCLK_STATUS_TEXT[status]);
}
