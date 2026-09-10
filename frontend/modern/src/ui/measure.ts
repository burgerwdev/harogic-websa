// Measurement mode state machine: on/off, tab, view switching
import * as S from '../core/store';
import { renderAll } from '../render/spectrum';
import { send } from '../core/wsSend';
import { measureAmp } from '../meas/amplitude';
import { measureChannel, clearChannel, updateChanTable, syncChanTableVisibility } from '../meas/channel';
import { measHarmApply } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';
import { updateInfoBar } from '../render/infobar';
import { t } from '../core/i18n';

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

export function setMeasButtons(en: boolean) {
  ['btn-amp-meas', 'btn-amp-clear', 'btn-harm-set', 'btn-harm-mode',
    'btn-pnm-set', 'btn-pnm-apply', 'btn-chan-meas', 'btn-chan-clear'].forEach(id => {
    const b = document.getElementById(id) as HTMLButtonElement;
    if (b) b.disabled = !en;
  });
  // modern buttons may have different ids; also support the data-action approach
  const acts = ['meas-amp', 'clear-amp', 'meas-harm', 'meas-pnm', 'meas-chan', 'clear-chan'];
  acts.forEach(a => {
    const el = document.querySelector(`[data-action="${a}"]`) as HTMLButtonElement;
    if (el) el.disabled = !en;
  });
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
  if (S.viewMode !== 'std') {
    S.setViewMode('std');
    const mt = document.getElementById('marker-table');
    if (mt) mt.style.display = '';
    const ht = document.getElementById('harmonic-table');
    if (ht) ht.style.display = 'none';
    const pt = document.getElementById('pnm-table');
    if (pt) pt.style.display = 'none';
  }
  applyMeasUI();
  renderAll();
}

export function applyMeasUI() {
  const inMeas = S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm');
  document.body.classList.toggle('meas-mode', inMeas);
  const chanTab = S.measOn && S.measTabSel === 'chan';
  const mt = document.getElementById('marker-table');
  if (mt) mt.style.display = ((S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm')) || chanTab) ? 'none' : '';
  syncChanTableVisibility();
  const ht = document.getElementById('harmonic-table');
  if (ht) ht.style.display = (S.measOn && S.viewMode === 'harm') ? '' : 'none';
  const pt = document.getElementById('pnm-table');
  if (pt) pt.style.display = (S.measOn && S.viewMode === 'pnm') ? '' : 'none';
  if ((S.viewMode === 'harm' || S.viewMode === 'pnm') && !S.stdSnap) {
    S.setStdSnap({ traces: S.traces.map(t => ({ mode: t.mode })), markers: S.markers.map(m => Object.assign({}, m)) });
  }
}

export { updateInfoBar };
