// Marker panel: selection, peak/valley navigation, centring and tracking.
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { markerFreqHz } from '../../core/markerCommon';
import { getTheme } from '../../core/theme';
import { send } from '../../core/wsSend';
import { assignMarkerToBestPeak, toggleMarkerTracking } from '../../dsp/markerTracking';
import { findExtremesOrdered, getDisplayPowers, nextExtreme, parabolaFit, setMarkerIdx } from '../../dsp/peaks';
import { nearestIdxAtX } from '../../render/plot';
import { requestRender } from '../../render/redraw';
import { currentGraphMode } from '../graphMode';
import { valleySeqPos } from '../measurePrefs';
import { updateFreqUIInputs } from '../freqInputs';
import { sdrCenterHz, sdrDecimate } from '../sdrState';
import { spanHz } from '../freqState';
import { beginFrequencyCommit } from './frequency';
import { beginRtaAxisTransition } from './rta';

export function activeMarkerPeak() {
  const p = getDisplayPowers(); if (!p) return;
  let bi = 0, bv = -Infinity;
  for (let i = 0; i < p.length; i++) { const v = p[i]; if (isFinite(v) && v > bv) { bv = v; bi = i; } }
  setMarkerIdx(bi);
}

export function activeMarkerValley() {
  const p = getDisplayPowers(); if (!p) return;
  let bi = 0, bv = Infinity;
  for (let i = 0; i < p.length; i++) { const v = p[i]; if (isFinite(v) && v < bv) { bv = v; bi = i; } }
  const list = findExtremesOrdered('right', false);
  valleySeqPos.set(list.findIndex((x: any) => Math.abs(x.i - bi) <= 3));
  if (valleySeqPos.get() < 0) valleySeqPos.set(0);
  const fit = parabolaFit(p, bi);
  const bh = (S.freqArray && S.freqArray!.length > 1) ? S.freqArray![1] - S.freqArray![0] : 0;
  setMarkerIdx(bi, S.freqArray![bi] + fit.dk * bh);
}

export function activeMarkerNextPeakLeft() { nextExtreme('left', true); }

export function activeMarkerNextPeakRight() { nextExtreme('right', true); }

export function activeMarkerNextValleyLeft() { nextExtreme('left', false); }

export function activeMarkerNextValleyRight() { nextExtreme('right', false); }

export function markerToCenter() {
  const m = S.markers.find(x => x.id === S.activeMkrId);
  if (!m || !m.enabled) return;
  const center = markerFreqHz(m.idx);
  // S.rtaMode includes SDR; use the graph mode to select the backend command.
  if (currentGraphMode() === 'sdr') {
    // SET_SDR retunes the demod to the new centre, so the listen frequency follows the
    // marked signal — the same "put this signal at the centre" semantics as in SWP.
    sdrCenterHz.set(center);
    send({ cmd: 'SET_SDR', center, decimate: Math.round(sdrDecimate.get()) });
  } else if (currentGraphMode() === 'rta') {
    const request_id = beginFrequencyCommit('rta-freq-settings', { center, span: spanHz.get() });
    beginRtaAxisTransition(request_id);
    send({ cmd: 'SET_RTA', center, request_id });
  } else {
    const window = { center, span: spanHz.get() };
    const request_id = beginFrequencyCommit('swp-freq-settings', window);
    updateFreqUIInputs();
    send({ cmd: 'SET_FREQ', ...window, request_id });
  }
}

export function selectMarker(id: number) {
  S.setActiveMkrId(id);
  document.querySelectorAll('.mkr-btn').forEach(b => b.classList.remove('active'));
  const b = document.querySelector(`.mkr-btn.M${id}`);
  if (b) b.classList.add('active');
  const m = S.markers.find(x => x.id === id);
  if (!m) return;
  m.enabled = true;
  if (m.mode === 'OFF') m.mode = 'NORMAL';
  autoTrackMarker(m);
  updateMarkersAllBtn();
  syncMarkerTrackingToggle();
  requestRender();
}

export function autoTrackMarker(m: S.MarkerState) {
  assignMarkerToBestPeak(m);
}

export function syncMarkerTrackingToggle() {
  const marker = S.markers.find(item => item.id === S.activeMkrId);
  const button = document.getElementById('btn-marker-tracking');
  if (!marker || !button) return;
  button.textContent = t('tracking');
  button.classList.toggle('active', marker.tracking);
  button.setAttribute('aria-pressed', String(marker.tracking));
}

export function toggleActiveMarkerTracking() {
  const marker = S.markers.find(item => item.id === S.activeMkrId);
  if (!marker) return;
  toggleMarkerTracking(marker);
  syncMarkerTrackingToggle();
  requestRender();
}

export function updateMarkersAllBtn() {
  const allOn = S.markers.every(m => m.enabled && m.mode !== 'OFF');
  const el = document.getElementById('btn-markers-all');
  if (el) {
    el.textContent = allOn ? t('all_on') : t('all_off');
    el.classList.toggle('active', allOn);
  }
}

export function toggleMarkersAll() {
  const allOn = S.markers.every(m => m.enabled && m.mode !== 'OFF');
  if (allOn) {
    S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; });
  } else {
    // All on: track one peak at a time — M1 takes the strongest peak, later markers skip
    // occupied positions and take the next strongest
    // (consistent with the autoTrackMarker logic in selectMarker)
    S.markers.forEach(m => { m.enabled = true; if (m.mode === 'OFF') m.mode = 'NORMAL'; });
    S.markers.forEach(m => autoTrackMarker(m));
  }
  updateMarkersAllBtn();
  const themeBtn = document.getElementById('btn-theme');
  if (themeBtn) themeBtn.textContent = getTheme() === 'dark' ? t('dark') : t('light');
  syncMarkerTrackingToggle();
  requestRender();
}

export function placeMarkerFromX(x: number, p: Float32Array) {
  // Search the drawn native-Hz positions in both full capture and local zoom.
  setMarkerIdx(nearestIdxAtX(x, p.length));
}
