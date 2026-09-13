// Marker panel: selection, peak/valley navigation, centring and tracking.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { markerFreqHz } from '../../core/markerCommon';
import { getTheme } from '../../core/theme';
import { send } from '../../core/wsSend';
import { assignMarkerToBestPeak, toggleMarkerTracking } from '../../dsp/markerTracking';
import { findExtremesOrdered, getDisplayPowers, nextExtreme, parabolaFit, setMarkerIdx } from '../../dsp/peaks';
import { plotRect } from '../../render/plot';
import { requestRender } from '../../render/redraw';
import { updateFreqUIInputs } from '../freqInputs';
import { centerHz, rtaCenterHz, spanHz } from '../freqState';

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
  S.setValleySeqPos(list.findIndex((x: any) => Math.abs(x.i - bi) <= 3));
  if (S.valleySeqPos < 0) S.setValleySeqPos(0);
  const fit = parabolaFit(p, bi);
  const bh = (S.freqArray && S.freqArray.length > 1) ? S.freqArray[1] - S.freqArray[0] : 0;
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
  if (S.rtaMode) {
    rtaCenterHz.set(center);
    send({ cmd: 'SET_RTA', center });
  } else {
    centerHz.set(center);
    updateFreqUIInputs();
    send({ cmd: 'SET_FREQ', center: centerHz.get(), span: spanHz.get() });
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
  const pr = plotRect();
  const frac = (x - pr.x) / pr.w;
  setMarkerIdx(Math.round(frac * (p.length - 1)));
}
