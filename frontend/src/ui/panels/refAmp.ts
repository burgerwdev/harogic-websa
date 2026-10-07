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

/**
 * The level a USER may ask for, in every mode.
 *
 * Deliberately not the device's Ref row reported in STATUS `caps`: the vendor SDK documents no Ref
 * range, and the device programs what its attenuation/IF-gain combination can do and echoes that
 * back (measured on this bench: -140 dBm is programmed exactly, +35 dBm comes back as +27). That row
 * is what the auto-placement rules use, so clamping user input to it refused levels the instrument
 * handles - the official software lets the level move further and only hints when the IF saturates
 * (`!IF overflow`, the -12 warning). These numbers mirror the backend's own bound for `SET_REF.ref`
 * and `AUTO_SCALE.current_ref` (config.DISPLAY_REF_*_DBM).
 */
const REF_USER_MIN = -160;
const REF_USER_MAX = 40;

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  const value = parseFloat(el.value);
  if (!isFinite(value)) return;
  if (currentGraphMode() === 'sdr') {
    // SDR Ref controls the IQS hardware reference level as well as the display. The
    // backend reconfigures IQS and applies the normal audio reset/fade sequence.
    const display = Math.max(REF_USER_MIN, Math.min(REF_USER_MAX, value));
    clearAutoScaleHint();
    setDisplayRef('sdr', display);        // client-owned scale: see DisplayRefSource
    syncSdrRefUI();                       // the Auto button must reflect the real state
    const cv = document.getElementById('spectrum');
    if (cv) cv.dataset.sdrRef = String(Math.round(display));
    prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: display });
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
    const next = Math.max(REF_USER_MIN, Math.min(REF_USER_MAX, getDisplayRef() + direction * S.dbPerDiv));
    clearAutoScaleHint();
    setDisplayRef('sdr', next);
    syncSdrRefUI();                       // ditto
    const cv = document.getElementById('spectrum');
    if (cv) cv.dataset.sdrRef = String(Math.round(next));
    prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: next });
    requestRender();
    return;
  }
  const base = refLevel.get();
  // Stepped to the end of the USER range (see REF_USER_*): a press past it is a quiet no-op, not a
  // refusal, and the device's own echo decides what the level really became.
  const next = steppedRefLevel(base, refStepDbm(), direction, REF_USER_MIN, REF_USER_MAX);
  if (next === base) return;
  // The stepped value is an intent: rendered immediately and dropped by the slot's TTL if
  // the backend never accepts it (the old code hand-rolled exactly this with refPending).
  refLevel.set(next);
  send({ cmd: 'SET_REF', mode: 'manual', ref: next });
}

/**
 * One Level-offset value from a gesture (the level axis in the RELATIVE display, where the top of
 * the graticule is a rendering offset rather than a device reference).
 *
 * The slot, the box and the repaint move together: a value changed behind the panel's back would
 * leave the box showing an offset that is not the one on screen.
 */
export function commitLevelOffset(value: number): void {
  if (!isFinite(value)) return;
  displayOffset.set(value);
  const box = document.getElementById('input-offset') as HTMLInputElement | null;
  if (box && document.activeElement !== box) box.value = String(Number(value.toFixed(1)));
  requestRender();
}

/**
 * One absolute Ref request from a gesture (the level axis drag), and the value that was asked for.
 *
 * The same paths as the Set button and the step arrows, with the level clamped to the OWNER's
 * range instead of reported: an end of the range is a no-op, not a message (the clamp notice was
 * removed on request). Returns the requested value so a caller that previews can wait for it, or
 * null when the value is not usable.
 */
export function commitRefLevel(value: number): number | null {
  if (!isFinite(value)) return null;
  if (currentGraphMode() === 'sdr') {
    const next = Math.max(REF_USER_MIN, Math.min(REF_USER_MAX, value));
    clearAutoScaleHint();
    setDisplayRef('sdr', next);
    syncSdrRefUI();
    const cv = document.getElementById('spectrum');
    if (cv) cv.dataset.sdrRef = String(Math.round(next));
    prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: next });
    return next;
  }
  const next = Math.max(REF_USER_MIN, Math.min(REF_USER_MAX, value));
  refLevel.set(next);
  send({ cmd: 'SET_REF', mode: 'manual', ref: next });
  return next;
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
  // The step arrows stay usable at the ends of the SDR range as well (same rule as the swept
  // panel): the level is clamped to the range silently by adjustRefLevel.
  const down = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  if (down) down.title = t('ref_down');
  const up = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (up) up.title = t('ref_up');
}
