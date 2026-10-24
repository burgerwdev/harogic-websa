/**
 * Measurement-panel visibility (leaf).
 *
 * Shared visibility helpers for the measurement state machine and completed measurements.
 */
import * as S from '../core/store';
import { t } from '../core/i18n';
import { syncChanTableVisibility } from '../meas/channel';
import { setWaterfall } from './panels/waterfall';
import { waterfallOn } from './waterfallState';
import { syncSpectrumZoomUi } from './spectrumZoomUi';
import { measurementsAvailable } from './measurementAvailability';

//: Waterfall state before the measurement took it over (null = not measuring).
let waterfallBeforeMeasure: boolean | null = null;

/**
 * A measurement owns the display: the waterfall panel replaces the result table, so it is
 * turned off and disabled while any measurement is active, and the user's previous choice is
 * restored when the measurement ends.
 */
function syncWaterfallAvailability(): void {
  const button = document.getElementById('btn-waterfall') as HTMLButtonElement | null;
  const pause = document.getElementById('btn-wf-pause') as HTMLButtonElement | null;
  if (S.measOn && measurementsAvailable()) {
    if (waterfallBeforeMeasure === null) waterfallBeforeMeasure = waterfallOn.get();
    if (waterfallOn.get()) setWaterfall(false);
    if (button) {
      button.disabled = true;
      button.title = t('tip_wf_meas');
    }
    if (pause) pause.disabled = true;
  } else {
    if (button) button.disabled = false;
    if (pause) pause.disabled = false;
    if (waterfallBeforeMeasure) setWaterfall(true);
    waterfallBeforeMeasure = null;
  }
}

export function setMeasButtons(en: boolean) {
  const available = measurementsAvailable();
  document.querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLSelectElement>(
    '#meas-panel button, #meas-panel input, #meas-panel select',
  ).forEach(control => { control.disabled = !available; });
  const acts = ['meas-amp', 'clear-amp', 'meas-harm', 'meas-pnm', 'meas-chan', 'clear-chan', 'harm-mode'];
  acts.forEach(action => {
    document.querySelectorAll<HTMLButtonElement>(`[data-action="${action}"]`).forEach(button => {
      button.disabled = !available || !en;
    });
  });
}

export function applyMeasUI() {
  setMeasButtons(S.measOn);
  syncWaterfallAvailability();
  // PNM has no linear spectrum axis: the zoom button must disable the moment a
  // measurement (and specifically PNM) takes over (design §3).
  syncSpectrumZoomUi(true);
  const inMeas = S.measOn && (S.viewMode === 'harm' || S.viewMode === 'pnm');
  document.body.classList.toggle('meas-mode', inMeas);
  const chanTab = S.measOn && S.measTabSel === 'chan';
  const mt = document.getElementById('marker-table');
  // STATUS refreshes this UI between render frames; it must keep the waterfall's table
  // replacement in effect or the spectrum resizes twice for every report.
  if (mt) mt.style.display = (waterfallOn.get() || inMeas || chanTab) ? 'none' : '';
  syncChanTableVisibility();
  const ht = document.getElementById('harmonic-table');
  if (ht) ht.style.display = (S.measOn && S.viewMode === 'harm') ? '' : 'none';
  const pt = document.getElementById('pnm-table');
  if (pt) pt.style.display = (S.measOn && S.viewMode === 'pnm') ? '' : 'none';
  if ((S.viewMode === 'harm' || S.viewMode === 'pnm') && !S.stdSnap) {
    S.setStdSnap({ traces: S.traces.map(t => ({ mode: t.mode })), markers: S.markers.map(m => Object.assign({}, m)) });
  }
}
