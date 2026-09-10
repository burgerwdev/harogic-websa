// Reference-clock status derivation + subtle box flash on apply (approach C2).
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

/**
 * Compare the requested source with the source the device actually reports.
 * 'fallback' means External was requested but the device runs on Internal (unlocked input).
 * Lock *quality* is often unavailable, so confirmation is limited to acceptance.
 */
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

const STATUS_KEYS: Record<RefClockStatus, string> = {
  applied: 'refclk_applied',
  fallback: 'refclk_fallback',
  forced: 'refclk_forced',
  unverified: 'refclk_unverified',
};

function sourceLabel(src: number | null | undefined): string {
  const name = refClockSourceName(src);
  if (name === 'external_forced') return t('ext_force');
  if (name === 'premium') return 'Int+ (DOCXO)';
  if (name === 'external') return t('ext');
  if (name === 'internal') return t('int');
  return '—';
}

function requestedSrcCode(requested: string): number {
  if (requested === 'external') return 1;
  if (requested === 'external_forced') return 3;
  if (requested === 'premium') return 2;
  return 0;
}

function activeActual(status: any): { src: unknown; freq: unknown } {
  const actual = status?.mode === 'rta' ? status?.rta_actual : status?.actual;
  return { src: actual?.refclk_src, freq: actual?.refclk };
}

// Flash the box that contains the reference-clock controls (subtle, style-consistent).
let flashTimer: number | null = null;

export function flashRefClockBox(status: RefClockStatus): void {
  const select = document.getElementById('select-refclk');
  const box = select?.closest('.info-item') as HTMLElement | null;
  if (!box) return;
  box.classList.remove('refclk-flash', 'warn');
  // Force reflow so the animation restarts on consecutive switches.
  void box.offsetWidth;
  box.classList.add('refclk-flash');
  if (status === 'fallback' || status === 'forced') box.classList.add('warn');
  if (flashTimer !== null) window.clearTimeout(flashTimer);
  flashTimer = window.setTimeout(() => {
    box.classList.remove('refclk-flash', 'warn');
    flashTimer = null;
  }, 1500);
}

/** Update the hover tooltip, and flash the control box when a command response arrives. */
export function refreshRefClockHint(status: any, responseTo?: string): void {
  const { src, freq } = activeActual(status);
  const requested = String(status?.ref_clock ?? 'internal');
  const state = refClockStatus(requested, src as number | null);
  const ppm = Number(status?.refclk_ppm);
  const ppmText = isFinite(ppm) && ppm !== 0 ? `${ppm.toFixed(2)} ppm` : '—';
  const freqText = isFinite(Number(freq)) && Number(freq) > 0
    ? `${(Number(freq) / 1e6).toFixed(4)} MHz` : '—';
  const outText = status?.refclk_out ? t('on') : t('off');

  const lines = [
    `${t('tip_refclk_requested')}: ${sourceLabel(requestedSrcCode(requested))}`,
    `${t('tip_refclk_actual')}: ${sourceLabel(src as number | null)}`,
    `${t('tip_refclk_freq')}: ${freqText}`,
    `${t('tip_refclk_ppm')}: ${ppmText}`,
    `${t('tip_refclk_output')}: ${outText}`,
    `${t('tip_refclk_state')}: ${t(STATUS_KEYS[state])}`,
  ];
  if (requested === 'external_forced') lines.push(t('tip_refclk_forced_warn'));
  const tooltip = lines.join('\n');
  const select = document.getElementById('select-refclk');
  if (select) select.title = tooltip;
  const out = document.getElementById('btn-refclk-out');
  if (out) out.title = tooltip;

  if (responseTo === 'SET_REFCK' || responseTo === 'SET_REFCKOUT') {
    flashRefClockBox(state);
  }
}
