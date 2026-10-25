// Reference/amplitude panel: Ref Level, scale, attenuation/preamp/IF gain, offset,
// reference clock and gap fill.
import * as S from '../../core/store';
import { prepareSdrAudioTransition } from '../../audio/sdrAudio';
import { steppedRefLevel } from '../../core/frequency';
import { t } from '../../core/i18n';
import { createParam } from '../../core/params';
import { send } from '../../core/wsSend';
import { updateInfoBar } from '../../render/infobar';
import { requestRender } from '../../render/redraw';
import { clampDisplayRef, displayRefDiverges, displayRefMin, displayRefMax, hardwareRefForDisplay, needsHardwareRef, getDisplayRef, rawToDisplayRef, rejectDisplayRefRequest, setDisplayRef, shiftDisplayRef } from '../displayRef';
import { currentGraphMode, measurementOwnsDevice } from '../graphMode';
import { refLevel } from '../refState';
import { autoScaleBusy, autoScaleRequest, clearAutoScaleHint } from '../refAutoScale';
import { displayOffset, displayUnit } from '../displayState';
import { syncWfRangeUi } from '../wfRange';
import { AMP_FIELDS, type AmpField, setAmpField, syncAmpAvailability } from '../ampState';

export function setRefLevel() {
  const el = document.getElementById('input-ref') as HTMLInputElement;
  commitRefLevel(parseFloat(el.value));
}

export function refStepDbm(): number {
  // One full grid division: ▲/▼ moves Ref by the current dB-per-division value.
  return S.dbPerDiv;
}

export function adjustRefLevel(direction: -1 | 1) {
  const base = getDisplayRef();
  const next = steppedRefLevel(base, refStepDbm(), direction, displayRefMin(), displayRefMax());
  if (next !== base) commitRefLevel(next);
}

/** Apply external gain/loss compensation, or pan the relative display's level offset. */
export function commitLevelOffset(value: number): void {
  if (!isFinite(value)) return;
  const delta = value - displayOffset.get();
  displayOffset.set(value);
  if (displayUnit.get() !== 'dB') {
    shiftDisplayRef(delta);
  }
  syncRefBounds();
  syncRefUI(true);
  const box = document.getElementById('input-offset') as HTMLInputElement | null;
  if (box && document.activeElement !== box) box.value = String(Number(value.toFixed(1)));
  updateInfoBar();
  requestRender();
}

/**
 * One absolute Ref request from a gesture (the level axis drag), and the value that was asked for.
 *
 * Clamp to owner bounds; endpoints are silent no-ops. Return the requested value for preview,
 * or null when the value is unusable.
 */
export function commitRefLevel(value: number): number | null {
  if (!isFinite(value) || measurementOwnsDevice()) return null;
  const next = clampDisplayRef(value);
  const raw = hardwareRefForDisplay(next);
  const configure = needsHardwareRef(next);
  clearAutoScaleHint();
  setDisplayRef('user', next);
  if (configure) {
    refLevel.set(raw);
    if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
    send({ cmd: 'SET_REF', mode: 'manual', ref: raw });
  }
  syncRefUI(true);
  updateInfoBar();
  requestRender();
  return next;
}

/** Command errors carry their originating command; unrelated stream errors cannot cancel Ref. */
export function rejectManualRef(): void {
  if (rejectDisplayRefRequest()) refLevel.reset();
  syncRefUI(true);
  updateInfoBar();
  requestRender();
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

const refClockSource = createParam<string>('clock.source', {
  fallback: 'internal', scope: 'clock', ttlMs: Infinity,
});
const refClockOutput = createParam<boolean>('clock.output', {
  fallback: false, scope: 'clock', ttlMs: Infinity,
});
let clockRequestSequence = 0;
let sourceRequestId: string | null = null;
let outputRequestId: string | null = null;

interface RefClockReport {
  ref_clock?: string;
  refclk_out?: boolean;
  response_to?: string;
  request_id?: string | number;
}

function renderRefClockIntent(): void {
  const source = document.getElementById('select-refclk') as HTMLSelectElement | null;
  if (source) {
    source.value = refClockSource.get();
    source.dataset.pending = String(refClockSource.pending());
    source.setAttribute('aria-busy', String(refClockSource.pending()));
  }
  const on = refClockOutput.get();
  const btn = document.getElementById('btn-refclk-out');
  if (btn) {
    btn.classList.toggle('on', on);
    btn.classList.toggle('pending', refClockOutput.pending());
    btn.setAttribute('aria-busy', String(refClockOutput.pending()));
    btn.setAttribute('aria-pressed', String(on));
    btn.textContent = t('output') + ': ' + t(on ? 'on' : 'off');
  }
  document.getElementById('refclk-label')?.classList.toggle('refclk-label-on', on);
}

export function setRefClock(mode: string) {
  if (measurementOwnsDevice()) return;
  refClockSource.set(mode);
  sourceRequestId = `clock-source-${++clockRequestSequence}`;
  renderRefClockIntent();
  // In SDR the clock reconfigures the IQS capture chain; mute its transition first.
  if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
  send({ cmd: 'SET_REFCK', mode, request_id: sourceRequestId });
}

export function toggleRefClkOut() {
  if (measurementOwnsDevice()) return;
  const on = !refClockOutput.get();
  refClockOutput.set(on);
  outputRequestId = `clock-output-${++clockRequestSequence}`;
  renderRefClockIntent();
  if (currentGraphMode() === 'sdr') prepareSdrAudioTransition();
  send({ cmd: 'SET_REFCKOUT', on, request_id: outputRequestId });
}

/** Readback may update rollback state, but only the newest correlated ACK settles intent. */
export function syncRefClockStatus(
  status: RefClockReport,
  responseTo = status.response_to,
  requestId = status.request_id,
): void {
  const clockReply = responseTo === 'SET_REFCK' || responseTo === 'SET_REFCKOUT';
  if (typeof status.ref_clock === 'string') {
    const desired = refClockSource.desiredValue();
    const matching = responseTo === 'SET_REFCK' && requestId === sourceRequestId;
    const stale = clockReply && sourceRequestId !== null && !matching;
    if (!stale || desired !== null) {
      refClockSource.confirm(status.ref_clock);
      if (desired !== null) {
        if (matching) refClockSource.reset();
        else refClockSource.set(desired);
      }
    }
  }
  if (typeof status.refclk_out === 'boolean') {
    const desired = refClockOutput.desiredValue();
    const matching = responseTo === 'SET_REFCKOUT' && requestId === outputRequestId;
    const stale = clockReply && outputRequestId !== null && !matching;
    if (!stale || desired !== null) {
      refClockOutput.confirm(status.refclk_out);
      if (desired !== null) {
        if (matching) refClockOutput.reset();
        else refClockOutput.set(desired);
      }
    }
  }
  renderRefClockIntent();
}

export function rejectRefClockRequest(command: string, requestId?: string | number): void {
  if (command === 'SET_REFCK' && requestId === sourceRequestId) {
    refClockSource.reset();
    sourceRequestId = `clock-source-${++clockRequestSequence}`;
  }
  if (command === 'SET_REFCKOUT' && requestId === outputRequestId) {
    refClockOutput.reset();
    outputRequestId = `clock-output-${++clockRequestSequence}`;
  }
  renderRefClockIntent();
}

/** Preset/disconnect: cancel outstanding intents without reusing their correlation IDs. */
export function resetRefClockIntent(): void {
  refClockSource.reset();
  refClockOutput.reset();
  sourceRequestId = `clock-source-${++clockRequestSequence}`;
  outputRequestId = `clock-output-${++clockRequestSequence}`;
  renderRefClockIntent();
}

export function setAmp(element: HTMLSelectElement) {
  if (measurementOwnsDevice()) return;
  const field = (Object.keys(AMP_FIELDS) as AmpField[]).find(key => AMP_FIELDS[key].id === element.id);
  if (field) setAmpField(field, element);
}

export function setOffset() {
  const v = parseFloat((document.getElementById('input-offset') as HTMLInputElement).value);
  commitLevelOffset(isFinite(v) ? v : 0);
}

export function toggleGapFill() {
  S.setCurrentGapFill(!S.currentGapFill);
  const btn = document.getElementById('btn-gapfill');
  if (btn) {
    btn.textContent = S.currentGapFill ? t('on') : t('off');
    btn.classList.toggle('active', S.currentGapFill);
  }
}

/** Measurement sessions own the RF controls and their incompatible display windows. */
export function syncLevelAvailability(mode: string): void {
  syncRefBounds();
  const exclusive = mode === 'harmonic' || mode === 'pnm';
  const panel = document.getElementById('level-panel');
  if (panel) {
    panel.setAttribute('aria-disabled', String(exclusive));
    panel.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button')
      .forEach(control => { control.disabled = exclusive; });
  }
  syncAmpAvailability(exclusive);
  syncWfRangeUi();
}

/** The editor remains compensated dBm regardless of the readout's amplitude unit. */
export function syncRefBounds(): void {
  const input = document.getElementById('input-ref') as HTMLInputElement | null;
  if (input) {
    input.min = String(displayRefMin());
    input.max = String(displayRefMax());
  }
}

export function syncRefUI(force = false) {
  syncRefBounds();
  const b = document.getElementById('btn-ref-auto');
  if (b) {
    b.classList.toggle('busy', autoScaleBusy());
    b.title = t('tip_btn-ref-auto');
  }
  const inp = document.getElementById('input-ref') as HTMLInputElement | null;
  if (inp) {
    inp.disabled = measurementOwnsDevice();
    // Discipline 4: the user can see that the device has not confirmed the new level yet.
    inp.classList.toggle('ref-pending', displayRefDiverges());
    const ref = displayUnit.get() === 'dB' ? rawToDisplayRef(refLevel.get()) : getDisplayRef();
    if (force || document.activeElement !== inp) inp.value = String(Number(ref.toFixed(1)));
  }
  const cv = document.getElementById('spectrum');
  if (cv && currentGraphMode() === 'sdr') cv.dataset.sdrRef = String(Math.round(getDisplayRef()));
  const setBtn = document.getElementById('btn-ref-set') as HTMLButtonElement | null;
  if (setBtn) setBtn.disabled = measurementOwnsDevice();
  // The step arrows stay usable at the ends of the SDR range as well (same rule as the swept
  // panel): the level is clamped to the range silently by adjustRefLevel.
  const down = document.getElementById('btn-ref-down') as HTMLButtonElement | null;
  if (down) down.title = t('ref_down');
  const up = document.getElementById('btn-ref-up') as HTMLButtonElement | null;
  if (up) up.title = t('ref_up');
}
