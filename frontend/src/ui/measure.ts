// Measurement mode state machine: on/off, tab, view switching
import * as S from '../core/store';
import { requestRender } from '../render/redraw';
import { send } from '../core/wsSend';
// Imported for their registration side effect (each module registers its own tab, E-5).
import '../meas/amplitude';
import { syncChanTableVisibility } from '../meas/channel';
import '../meas/channel';
import '../meas/harmonic';
import '../meas/phaseNoise';
import { updateInfoBar } from '../render/infobar';
import { t } from '../core/i18n';
import { getMeasurementTab, measurementTabs } from './measureRegistry';
import { applyMeasUI, setMeasButtons } from './measureUi';
import { measurementsAvailable } from './measurementAvailability';
import { endMeasurementSessions } from '../meas/session';
export { applyMeasUI, setMeasButtons } from './measureUi';

export function measToggle() {
  if (!S.measOn && !measurementsAvailable()) return;
  if (!S.measOn) {
    S.setMeasOn(true);
    const btn = document.getElementById('btn-meas-onoff');
    if (btn) btn.textContent = t('on');
    setMeasButtons(true);
    applyMeasTabUI();
    applyMeasNow();
  } else {
    S.setMeasOn(false);
    const btn = document.getElementById('btn-meas-onoff');
    if (btn) btn.textContent = t('off');
    setMeasButtons(false);
    exitMeasMode();
  }
}


export function applyMeasTabUI() {
  // Each registered tab owns its button and panel.
  const selected = S.measTabSel;
  for (const tab of measurementTabs()) {
    const button = document.getElementById(tab.domId);
    if (button) button.classList.toggle('active', selected === tab.id);
    const panel = document.getElementById(tab.domId.replace('tab-', 'meas-'));
    if (panel) panel.style.display = selected === tab.id ? '' : 'none';
  }
  syncChanTableVisibility();
}

export function applyMeasNow() {
  if (!measurementsAvailable()) return;
  exitMeasMode();
  // Unknown tab IDs have no action.
  getMeasurementTab(S.measTabSel)?.apply();
  applyMeasUI();
}

export function measTab(t: string) {
  if (!measurementsAvailable()) return;
  S.setMeasTabSel(t);
  applyMeasTabUI();
  if (!S.measOn) return;
  applyMeasNow();
}

export function exitMeasMode(updateBackend = true) {
  endMeasurementSessions();
  if (S.viewMode === 'pnm' || S.viewMode === 'harm') {
    if (updateBackend) send({ cmd: 'SET_MODE', mode: 'std' });
    if (S.stdSnap) {
      S.traces.forEach((t, i) => { if (S.stdSnap!.traces[i]) t.mode = S.stdSnap!.traces[i].mode; });
      S.markers.forEach((mk, i) => { if (S.stdSnap!.markers[i]) Object.assign(mk, S.stdSnap!.markers[i]); });
      S.setStdSnap(null);
    }
  }
  // Preset exits measurement views; RTA remains a normal analyzer view.
  if (S.viewMode === 'pnm' || S.viewMode === 'harm') {
    S.setViewMode('std');
    const mt = document.getElementById('marker-table');
    if (mt) mt.style.display = '';
    const ht = document.getElementById('harmonic-table');
    if (ht) ht.style.display = 'none';
    const pt = document.getElementById('pnm-table');
    if (pt) pt.style.display = 'none';
  }
  applyMeasUI();
  requestRender();
}


export { updateInfoBar };
