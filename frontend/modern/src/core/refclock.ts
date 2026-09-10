// Reference-clock status derivation + transient hint (approach C: no persistent chip).
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
 * Lock *quality* is often unavailable, so confirmation is deliberately limited to acceptance.
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

const STATUS_CLASS: Record<RefClockStatus, string> = {
  applied: 'ok',
  fallback: 'warn',
  forced: 'warn',
  unverified: 'dim',
};

function sourceLabel(src: number | null | undefined): string {
  const name = refClockSourceName(src);
  if (name === 'external_forced') return t('ext_force');
  if (name === 'premium') return 'Int+ (DOCXO)';
  if (name === 'external') return t('ext');
  if (name === 'internal') return t('int');
  return '—';
}

function activeActual(status: any): { src: unknown; freq: unknown } {
  const actual = status?.mode === 'rta' ? status?.rta_actual : status?.actual;
  return { src: actual?.refclk_src, freq: actual?.refclk };
}

let hintUntil = 0;
let hintTimer: number | null = null;

function hintEl(): HTMLElement | null {
  return document.getElementById('refclk-hint');
}

/** Show the transient "setting..." state right after a command is sent. */
export function pendingRefClockHint(): void {
  const el = hintEl();
  if (!el) return;
  el.textContent = t('refclk_pending') + '…';
  el.className = 'refclk-hint dim';
  hintUntil = Date.now() + 2000;
}

/** Refresh the hover tooltip always, and settle the transient hint after a response. */
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
    `${t('tip_refclk_requested')}: ${sourceLabel(
      requested === 'external_forced' ? 3 : requested === 'external' ? 1
        : requested === 'premium' ? 2 : 0)}`,
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

  const el = hintEl();
  if (!el) return;
  const isResponse = responseTo === 'SET_REFCK' || responseTo === 'SET_REFCKOUT';
  if (!isResponse && Date.now() > hintUntil) return;   // keep the steady tooltip only
  el.textContent = `${t('refclk_label')}: ${t(STATUS_KEYS[state])}`;
  el.className = `refclk-hint ${STATUS_CLASS[state]}`;
  hintUntil = Date.now() + 3000;
  if (hintTimer !== null) window.clearTimeout(hintTimer);
  hintTimer = window.setTimeout(() => {
    const node = hintEl();
    if (node) { node.textContent = ''; node.className = 'refclk-hint'; }
    hintTimer = null;
  }, 3000);
}
