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
import { refLevel } from '../refState';
import { autoScaleBusy, autoScaleRequest, clearAutoScaleHint } from '../refAutoScale';
import { displayOffset } from '../displayState';

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  const value = parseFloat(el.value);
  if (!isFinite(value)) return;
  if (currentGraphMode() === 'sdr') {
    // SDR Ref controls the IQS hardware reference level as well as the display. The
    // backend reconfigures IQS and applies the normal audio reset/fade sequence.
    clearAutoScaleHint();
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
    const next = Math.max(SDR_REF_MIN, Math.min(SDR_REF_MAX, getDisplayRef() + direction * S.dbPerDiv));
    clearAutoScaleHint();
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
  // The device's own range (STATUS caps), not a UI constant.
  const next = steppedRefLevel(base, refStepDbm(), direction, S.REF_MIN_DBM, S.REF_MAX_DBM);
  if (next === base) return;
  // The stepped value is an intent: rendered immediately and dropped by the slot's TTL if
  // the backend never accepts it (the old code hand-rolled exactly this with refPending).
  refLevel.set(next);
  send({ cmd: 'SET_REF', mode: 'manual', ref: next });
}

/**
 * Auto Scale: one action, not a mode. The fit (and its busy/result feedback) lives in
 * ui/refAutoScale.ts; this is the button's entry point.
 */
export function setRefAuto() {
  autoScaleRequest();
  requestRender();
}

export function setScale(v: number) {
  S.setDbPerDiv(v);
  syncScaleButtons();
  updateInfoBar();
  requestRender();
  // The window height changed, so a placement that was right is not right any more - but the
  // reference is the user's now: re-fit only when they ask (Auto Scale carries the new window).
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
  displayOffset.set(isFinite(v) ? v : 0);
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

// SDR's display scale belongs to the client, so its clamp is a client convention; the backend
// validates `AUTO_SCALE.current_ref` against the same numbers (config.DISPLAY_REF_*_DBM).
const SDR_REF_MIN = -160;
const SDR_REF_MAX = 40;

export function syncSdrRefUI() {
  if (currentGraphMode() !== 'sdr') return;
  const b = document.getElementById('btn-ref-auto');
  if (b) {
    b.classList.toggle('busy', autoScaleBusy());
    b.title = t('tip_btn-ref-auto');
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
  // The reference is never locked by a tracking mode any more, so the step buttons are always
  // usable. Range matches adjustRefLevel's SDR clamp.
  const ref = getDisplayRef();
  const down = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  if (down) {
    down.disabled = ref <= SDR_REF_MIN;
    down.title = t('ref_down');
  }
  const up = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (up) {
    up.disabled = ref >= SDR_REF_MAX;
    up.title = t('ref_up');
  }
}
