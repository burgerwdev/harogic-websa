// Measurement mode state machine: on/off, tab, view switching
import * as S from '../core/store';
import { requestRender } from '../render/redraw';
import { send } from '../core/wsSend';
import { measureAmp } from '../meas/amplitude';
import { measureChannel, updateChanTable, syncChanTableVisibility } from '../meas/channel';
import { measHarmApply } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';
import { updateInfoBar } from '../render/infobar';
import { t } from '../core/i18n';
import { applyMeasUI, setMeasButtons } from './measureUi';
export { applyMeasUI, setMeasButtons } from './measureUi';

export function measToggle() {
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
  const t = S.measTabSel;
  const tabAmp = document.getElementById('tab-amp');
  const tabHarm = document.getElementById('tab-harm');
  const tabPnm = document.getElementById('tab-pnm');
  const tabChan = document.getElementById('tab-chan');
  if (tabAmp) tabAmp.classList.toggle('active', t === 'amp');
  if (tabHarm) tabHarm.classList.toggle('active', t === 'harm');
  if (tabPnm) tabPnm.classList.toggle('active', t === 'pnm');
  if (tabChan) tabChan.classList.toggle('active', t === 'chan');
  const mAmp = document.getElementById('meas-amp');
  const mHarm = document.getElementById('meas-harm');
  const mPnm = document.getElementById('meas-pnm');
  const mChan = document.getElementById('meas-chan');
  if (mAmp) mAmp.style.display = t === 'amp' ? '' : 'none';
  if (mHarm) mHarm.style.display = t === 'harm' ? '' : 'none';
  if (mPnm) mPnm.style.display = t === 'pnm' ? '' : 'none';
  if (mChan) mChan.style.display = t === 'chan' ? '' : 'none';
  syncChanTableVisibility();
}

export function applyMeasNow() {
  exitMeasMode();
  if (S.measTabSel === 'amp') measureAmp();
  else if (S.measTabSel === 'harm') measHarmApply();
  else if (S.measTabSel === 'pnm') measPnmApply();
  else if (S.measTabSel === 'chan') { measureChannel(); updateChanTable(); }
  applyMeasUI();
}

export function measTab(t: string) {
  S.setMeasTabSel(t);
  applyMeasTabUI();
  if (!S.measOn) return;
  applyMeasNow();
}

export function exitMeasMode(updateBackend = true) {
  if (S.viewMode === 'pnm' || S.viewMode === 'harm') {
    if (updateBackend) send({ cmd: 'SET_MODE', mode: 'std' });
    if (S.stdSnap) {
      S.traces.forEach((t, i) => { if (S.stdSnap!.traces[i]) t.mode = S.stdSnap!.traces[i].mode; });
      S.markers.forEach((mk, i) => { if (S.stdSnap!.markers[i]) Object.assign(mk, S.stdSnap!.markers[i]); });
      S.setStdSnap(null);
    }
  }
  // Only the measurement views are left here: RTA is a normal analyzer view and must
  // survive (preset used to force view=std while the backend kept streaming RTA frames,
  // which blanked the spectrum).
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
