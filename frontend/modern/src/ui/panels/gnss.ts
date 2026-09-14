// GNSS detail popover.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { t } from '../../core/i18n';

function fmtNow(): string {
  const d = new Date();
  const p2 = (n: number) => String(n).padStart(2, '0');
  return d.getFullYear() + '-' + p2(d.getMonth() + 1) + '-' + p2(d.getDate()) + ' ' +
    p2(d.getHours()) + ':' + p2(d.getMinutes()) + ':' + p2(d.getSeconds());
}

export function fillGnssDetail() {
  const g = S.lastGnss || {};
  const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  set('gnss-d-lock', g.lock ? t('status_locked') : t('status_nolock'));
  set('gnss-d-sats', g.sats != null ? String(g.sats) : '-');
  set('gnss-d-docxo', g.docxo ? t('on') : t('off'));
  set('gnss-d-docxo_mode', g.docxo_mode === 0 ? t('gnss_docxo_lock') : (g.docxo_mode === 1 ? t('gnss_docxo_hold') : '-'));
  set('gnss-d-antenna', g.antenna === 0 ? t('gnss_ext_ant') : (g.antenna === 1 ? t('gnss_int_ant') : '-'));
  set('gnss-d-latitude', g.latitude != null && Math.abs(g.latitude) > 0.0001 ? g.latitude.toFixed(6) + '°' : '-');
  set('gnss-d-longitude', g.longitude != null && Math.abs(g.longitude) > 0.0001 ? g.longitude.toFixed(6) + '°' : '-');
  set('gnss-d-altitude', g.altitude != null ? String(g.altitude) + ' m' : '-');
  // Time: GNSS UTC (shown only when locked and valid) + system local time (always shown)
  set('gnss-d-utc', (g.lock && g.time && !g.time.includes('0000')) ? g.time : '-');
  set('gnss-d-local', fmtNow());
  const pop = document.getElementById('gnss-popover');
  if (pop) pop.style.display = '';
}

export function closeGnssDetail() {
  const pop = document.getElementById('gnss-popover');
  if (pop) pop.style.display = 'none';
}
