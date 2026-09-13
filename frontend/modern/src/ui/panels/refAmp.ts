// Reference/amplitude panel: Ref Level, scale, attenuation/preamp/IF gain, offset,
// reference clock and gap fill. Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { prepareSdrAudioTransition } from '../../audio/sdrAudio';
import { steppedRefLevel } from '../../core/frequency';
import { t } from '../../core/i18n';
import { send } from '../../core/wsSend';
import { updateInfoBar } from '../../render/infobar';
import { requestRender } from '../../render/redraw';
import { displayRefDiverges, getDisplayRef, setDisplayRef } from '../displayRef';
import { currentGraphMode } from '../graphMode';
import { refLevel, refMode } from '../refState';
import { sdrRefAuto } from '../sdrState';

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  const value = parseFloat(el.value);
  if (!isFinite(value)) return;
  if (currentGraphMode() === 'sdr') {
    // SDR Ref controls the IQS hardware reference level as well as the display. The
    // backend reconfigures IQS and applies the normal audio reset/fade sequence.
    sdrRefAuto.set(false);
    setDisplayRef('user', value);
    syncSdrRefUI();                       // the Auto button must reflect the real state
    const cv = document.getElementById('spectrum');
    if (cv) cv.dataset.sdrRef = String(Math.round(value));
    prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: value });
    requestRender();
    return;
  }
  send({ cmd: 'SET_REF', mode: 'manual', ref: value });
}

export function refStepDbm(): number {
  // One full grid division: ▲/▼ moves Ref by the current dB-per-division value.
  return S.dbPerDiv;
}

export function adjustRefLevel(direction: -1 | 1) {
  if (currentGraphMode() === 'sdr') {
    const next = Math.max(-160, Math.min(40, getDisplayRef() + direction * S.dbPerDiv));
    sdrRefAuto.set(false);
    setDisplayRef('user', next);
    syncSdrRefUI();                       // ditto
    const cv = document.getElementById('spectrum');
    if (cv) cv.dataset.sdrRef = String(Math.round(next));
    prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: next });
    requestRender();
    return;
  }
  const base = refLevel.get();
  const next = steppedRefLevel(base, refStepDbm(), direction, REF_MIN, REF_MAX);
  if (next === base) return;
  // The stepped value is an intent: rendered immediately and dropped by the slot's TTL if
  // the backend never accepts it (the old code hand-rolled exactly this with refPending).
  refLevel.set(next);
  send({ cmd: 'SET_REF', mode: 'manual', ref: next });
}

export function setRefAuto() {
  if (currentGraphMode() === 'sdr') {
    sdrRefAuto.set(!sdrRefAuto.get());
    syncSdrRefUI();
    requestRender();
    return;
  }
  if (refMode.get() === 'auto') {
    send({ cmd: 'SET_REF', mode: 'manual', ref: refLevel.get() });
  } else {
    // range_db = the visible window height. Auto Ref anchors the noise floor just above the
    // bottom of that window, so the backend needs to know how tall it is.
    send({ cmd: 'SET_REF', mode: 'auto', range_db: S.totalDivs * S.dbPerDiv });
  }
}

export function setScale(v: number) {
  S.setDbPerDiv(v);
  syncScaleButtons();
  updateInfoBar();
  requestRender();
  // The window height changed, so the auto-Ref target (noise floor just above the bottom)
  // changed too. Re-arm so the new spectrum lands correctly instead of keeping the old Ref.
  if (currentGraphMode() !== 'sdr' && refMode.get() === 'auto') {
    send({ cmd: 'SET_REF', mode: 'auto', range_db: S.totalDivs * S.dbPerDiv });
  }
}

export function syncScaleButtons() {
  const grp = document.getElementById('unit-scale-group');
  if (!grp) return;
  for (const b of grp.children) (b as HTMLElement).classList.toggle('active', parseFloat(b.textContent || '') === S.dbPerDiv);
}

export function setRefClock(mode: string) {
  // In SDR the reference clock lives in the IQS profile, so this reconfigures the
  // capture chain; mute first so the reconfiguration transient is not audible.
  if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
  send({ cmd: 'SET_REFCK', mode });
}

export function toggleRefClkOut() {
  const btn = document.getElementById('btn-refclk-out');
  const cur = btn && btn.classList.contains('on');
  if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
  send({ cmd: 'SET_REFCKOUT', on: !cur });
}

export function syncRefClkOut(s: any) {
  const btn = document.getElementById('btn-refclk-out');
  if (!btn) return;
  const on = !!s.refclk_out;
  btn.classList.toggle('on', on);
  btn.textContent = on ? (t('output') + ': ' + t('on')) : (t('output') + ': ' + t('off'));
}

export function setAmp() {
  if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
  send({
    cmd: 'SET_AMP',
    atten: parseInt((document.getElementById('select-atten') as HTMLSelectElement).value),
    preamp: parseInt((document.getElementById('select-preamp') as HTMLSelectElement).value),
    ifgain: parseInt((document.getElementById('select-ifgain') as HTMLSelectElement).value),
    gain_strategy: parseInt((document.getElementById('select-gainstrategy') as HTMLSelectElement).value),
  });
}

export function setOffset() {
  const v = parseFloat((document.getElementById('input-offset') as HTMLInputElement).value);
  S.setDisplayOffset(isFinite(v) ? v : 0);
  requestRender();
}

export function toggleGapFill() {
  S.setCurrentGapFill(!S.currentGapFill);
  const btn = document.getElementById('btn-gapfill');
  if (btn) {
    btn.textContent = S.currentGapFill ? t('on') : t('off');
    btn.classList.toggle('active', S.currentGapFill);
  }
}

// Ref Level bounds (moved with the panel).
const REF_MIN = -50;
const REF_MAX = 30;

export function syncSdrRefUI() {
  if (currentGraphMode() !== 'sdr') return;
  const b = document.getElementById('btn-ref-auto');
  if (b) {
    b.classList.toggle('active', sdrRefAuto.get());
    b.title = 'Auto amplitude reference';
  }
  const inp = document.getElementById('input-ref') as HTMLInputElement | null;
  if (inp) {
    inp.disabled = false;
    // Discipline 4: the user can see that the device has not confirmed the new level yet.
    inp.classList.toggle('ref-pending', displayRefDiverges());
    if (document.activeElement !== inp) inp.value = getDisplayRef().toFixed(0);
  }
  const setBtn = document.getElementById('btn-ref-set') as HTMLButtonElement | null;
  if (setBtn) setBtn.disabled = false;
  // In SDR the reference is owned by the client (auto-scale or manual), not by the backend
  // `ref_mode` the STATUS carries. Drive the step buttons from sdrRefAuto here, otherwise
  // toggling Auto off left them disabled until a Set (which is what made the backend report
  // manual). Range matches adjustRefLevel's SDR clamp.
  const ref = getDisplayRef();
  const down = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  if (down) {
    down.disabled = sdrRefAuto.get() || ref <= -160;
    down.title = sdrRefAuto.get() ? t('auto') : t('ref_down');
  }
  const up = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (up) {
    up.disabled = sdrRefAuto.get() || ref >= 40;
    up.title = sdrRefAuto.get() ? t('auto') : t('ref_up');
  }
}
