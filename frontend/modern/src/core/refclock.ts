// Reference-clock status: GNSS-style detail popover + subtle box flash on apply.
import { t } from './i18n';
import { send } from './wsSend';

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

let lastStatus: any = null;

/** Flash the box that contains the reference-clock controls. */
let flashTimer: number | null = null;

export function flashRefClockBox(status: RefClockStatus): void {
  const select = document.getElementById('select-refclk');
  const box = select?.closest('.info-item') as HTMLElement | null;
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

function setText(id: string, value: string): void {
  const el = document.getElementById(id);
  if (el) el.textContent = value;
}

export function fillRefClockDetail(): void {
  const st = lastStatus;
  if (!st) return;
  const { src, freq } = activeActual(st);
  const requested = String(st.ref_clock ?? 'internal');
  const state = refClockStatus(requested, src as number | null);
  const ppm = Number(st.refclk_ppm);
  const ppmText = isFinite(ppm) && ppm !== 0 ? `${ppm.toFixed(2)} ppm` : '—';
  const cal = Number(st.last_cal_freq);
  const gnss = st.gnss ?? {};

  setText('refclk-d-requested', sourceLabel(requestedSrcCode(requested)));
  setText('refclk-d-actual', sourceLabel(src as number | null));
  setText('refclk-d-freq', isFinite(Number(freq)) && Number(freq) > 0
    ? `${(Number(freq) / 1e6).toFixed(4)} MHz` : '—');
  setText('refclk-d-ppm', ppmText);
  setText('refclk-d-output', st.refclk_out ? t('on') : t('off'));
  setText('refclk-d-state', `${t(STATUS_KEYS[state])}${requested === 'external_forced'
    ? ` — ${t('tip_refclk_forced_warn')}` : ''}`);
  setText('refclk-d-gnss', gnss.lock
    ? `${t('status_locked')} (${gnss.sats ?? 0})` : t('status_nolock'));
  setText('refclk-d-cal', cal > 0 ? `${(cal / 1e6).toFixed(4)} MHz`
    : (st.calibrating ? '…' : '—'));

  const btn = document.getElementById('btn-refclk-cal') as HTMLButtonElement | null;
  if (btn) {
    btn.textContent = st.calibrating ? t('refclk_calibrating') : t('refclk_calibrate');
    btn.disabled = !!st.calibrating || !gnss.lock;
    btn.title = !gnss.lock ? t('refclk_cal_need_gnss') : '';
  }
}

/** Update the steady-state data and the flash; fill the popover when it is open. */
export function refreshRefClockHint(status: any, responseTo?: string): void {
  lastStatus = status;
  if (responseTo === 'SET_REFCK' || responseTo === 'SET_REFCKOUT') {
    const { src } = activeActual(status);
    flashRefClockBox(refClockStatus(String(status?.ref_clock ?? 'internal'),
      src as number | null));
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

/** GNSS 1PPS reference-clock calibration (backend runs it on a worker thread). */
export function calibrateRefClock(): void {
  const st = lastStatus;
  if (st && st.calibrating) return;
  send({ cmd: 'CAL_REFCLK', count: 10 });
  if (st) { st.calibrating = true; fillRefClockDetail(); }
}
