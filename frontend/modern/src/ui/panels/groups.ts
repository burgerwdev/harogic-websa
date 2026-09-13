// Collapsible control groups.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { syncMarkerTrackingToggle, updateMarkersAllBtn } from './markers';

export function syncToggleTexts() {
  const pl = document.getElementById('btn-peaklist');
  if (pl) pl.textContent = S.peakListOn ? t('on') : t('off');
  const gf = document.getElementById('btn-gapfill');
  if (gf) gf.textContent = S.currentGapFill ? t('on') : t('off');
  const mo = document.getElementById('btn-meas-onoff');
  if (mo) mo.textContent = S.measOn ? t('on') : t('off');
  const rc = document.getElementById('btn-refclk-out');
  if (rc) {
    const on = rc.classList.contains('on');
    rc.textContent = on ? (t('output') + ': ' + t('on')) : (t('output') + ': ' + t('off'));
  }
  updateMarkersAllBtn();
  syncMarkerTrackingToggle();
  const tracking = document.getElementById('btn-marker-tracking');
  if (tracking) tracking.textContent = t('tracking');
  const wb = document.getElementById('btn-waterfall');
  if (wb) wb.classList.toggle('active', S.waterfallOn);
}

export function toggleGroup(el: HTMLElement) {
  const g = el.closest('.control-group');
  if (g) {
    g.classList.toggle('collapsed');
    syncToggleIcons();
  }
}

export function toggleAllGroups() {
  const groups = document.querySelectorAll('.control-group');
  const allCollapsed = Array.from(groups).every(g => g.classList.contains('collapsed'));
  groups.forEach(g => g.classList.toggle('collapsed', !allCollapsed));
  syncToggleIcons();
}

export function syncToggleIcons() {
  const groups = Array.from(document.querySelectorAll('.control-group'));
  const allCollapsed = groups.length && groups.every(g => g.classList.contains('collapsed'));
  document.querySelectorAll('.global-toggle').forEach(b => { b.textContent = allCollapsed ? '^' : 'v'; });
  groups.forEach(g => {
    const b = g.querySelector('.panel-toggle');
    if (b) b.textContent = g.classList.contains('collapsed') ? '+' : '-';
  });
}
