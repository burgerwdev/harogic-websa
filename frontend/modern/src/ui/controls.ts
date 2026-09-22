// Control commands + data-action binding + panel collapse + marker ops + canvas interaction
import * as S from '../core/store';
import { send } from '../core/wsSend';

import { postRefNotice, requestSdrEntryFit, resetAutoScaleState } from './refAutoScale';
import { sdrAgc, sdrAudioOn, sdrCenterHz, sdrDecimate, sdrDeemph, sdrDemod, sdrIfbw, sdrListenHz, sdrNr, sdrNrStrength, sdrSpanHz, sdrSquelch, sdrVolume, estimatedCaptureSpanHz, hasStoredSdrPrefs, renderSdrState, resetSdrState } from './sdrState';
import { centerHz, swpCenterHz } from './freqState';
import { updateInfoBar } from '../render/infobar';
import { requestRender } from '../render/redraw';
import { getDisplayPowers } from '../dsp/peaks';

import { normRefWindow, setNormRefWinUser, smoothRefWindow, buildReferenceTablePub } from './normPub';
import { switchTraceTab, toggleFreeze, setTraceMode, clearRtaTrace, setTraceAverage, exportActiveTraceCsv, exportPeakListCsv } from './traceOps';
import { exportSpectrumPng } from './exportImage';
import { normalizeActiveTrace, resetActiveTraceNormalize } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
import { togglePeakList, peakThrManual, peakThrAuto, resetPeakThr } from '../render/peaklist';
import { measToggle, measTab, applyMeasUI, setMeasButtons } from './measure';
import { measureAmp, clearAmp } from '../meas/amplitude';
import { measureChannel, clearChannel } from '../meas/channel';
import { measHarmApply, autoHarmSpan } from '../meas/harmonic';
import { measPnmApply } from '../meas/phaseNoise';

import { t } from '../core/i18n';
import { getUiScale } from '../core/uiScale';
import { openRefClockDetail, closeRefClockDetail } from '../core/refclock';
import { audioSampleRate, prepareSdrAudioTransition, setSdrAudioEnabled } from '../audio/sdrAudio';
import { initSdrDemodGroup } from './sdrDemodGroup';
import { ft8DialFor } from '../sdr/ft8Log';
import { initFt8Window, setFt8WindowAvailable, toggleFt8Window } from './ft8Window';
import { dspLevelDbfs, resetSdrIq, setSdrDspAudioEnabled, setSdrIqEnabled, configureSdrPipeline, setSdrPipelineDeemph, setSdrPipelineNr, setSdrPipelineSquelch } from '../sdr/iqStream';
import { sdrModeIds } from '../sdr/registry';
import { resetLimits } from './limits';

// Panel modules (report finding P1-5). controls.ts keeps the wiring (event binding, canvas
// interaction, mode/preset orchestration) and imports the actions it dispatches; the
// re-exports below keep the previous public surface for the rest of the app.
import { applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetSpanStepAuto, stepSwpSpan, syncSwpSpanStep, updateCustomSpanStep } from './panels/frequency';
import { applyPoints, applyRBW, applyVBW, setSpurMode, setWindow } from './panels/resolution';
import { applyRta, clearRtaAccum, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins } from './panels/rta';
import { activeMarkerNextPeakLeft, activeMarkerNextPeakRight, activeMarkerNextValleyLeft, activeMarkerNextValleyRight, activeMarkerPeak, activeMarkerValley, markerToCenter, placeMarkerFromX, selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll } from './panels/markers';
import {
  adjustRefLevel, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel, setScale,
  syncSdrRefUI, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
import { resetWf, setSweepSpeed, syncSweepInput, toggleWaterfall, toggleWfPause } from './panels/waterfall';
import { closeGnssDetail, fillGnssDetail } from './panels/gnss';
import { toggleAllGroups, toggleGroup } from './panels/groups';
import { commitUnitField } from './panels/commit';

// ── Frequency linking ──

export function connectDevice() { send({ cmd: 'CONNECT' }); }

// ── Marker operations ──
// Graph-mode + display-reference requests are owned by ui/graphMode.ts and ui/displayRef.ts.
// They apply the four disciplines (id-matched ack, supersede, timeout notice, visible
// divergence); this module only translates them to the DOM.
import { currentGraphMode, graphModeDiverges, isGraphMode, pendingGraphMode, requestGraphMode, confirmGraphMode, resetGraphMode, setGraphModeTimeoutHandler } from './graphMode';
import { setDisplayRef, setDisplayRefTimeoutHandler } from './displayRef';

let sdrAudioHandoffTimer: number | null = null;

// Transient hint notices. A held notice (a timeout report) must not be overwritten by the
// high-frequency pending-state refresh, so it owns the field until it expires.
const hintHoldUntil = new Map<string, number>();
function setHint(id: string, text: string, holdMs = 0): void {
  const el = document.getElementById(id);
  if (!el) return;
  const now = Date.now();
  if (holdMs === 0 && (hintHoldUntil.get(id) ?? 0) > now) return;   // a held notice owns it
  if (holdMs > 0) hintHoldUntil.set(id, now + holdMs);
  else hintHoldUntil.delete(id);
  el.textContent = text;
}
function flashHint(id: string, text: string, holdMs = 5000): void {
  setHint(id, text, holdMs);
  window.setTimeout(() => {
    if ((hintHoldUntil.get(id) ?? 0) <= Date.now()) setHint(id, '');
  }, holdMs + 50);
}

setGraphModeTimeoutHandler((want) => {
  flashHint('mode-hint', t('mode_switch_timeout', { mode: want.toUpperCase() }));
  syncModeButtons();
});
setDisplayRefTimeoutHandler(() => {
  // Same place as the Auto Scale outcome: it is a message about the reference the user is
  // looking at, and the input already carries the local "unconfirmed" outline (ref-pending).
  postRefNotice(t('ref_switch_timeout'), 5000);
});

function deferSdrAudioPreference() {
  if (sdrAudioHandoffTimer !== null) window.clearTimeout(sdrAudioHandoffTimer);
  // Drop samples from the pre-SDR/default demod chain until the final SDR commands settle.
  setSdrAudioEnabled(false);
  sdrAudioHandoffTimer = window.setTimeout(() => {
    sdrAudioHandoffTimer = null;
    if (currentGraphMode() === 'sdr') applySdrAudioPreference();
  }, 800);
}

/** Mark the mode buttons while a request is unconfirmed. They stay enabled so that a new
 *  click supersedes the request instead of being swallowed (discipline 2 + 4). */
function syncModeButtons(): void {
  const want = pendingGraphMode();
  const busy = want !== null && graphModeDiverges();
  for (const id of ['btn-mode-rta', 'btn-mode-sdr']) {
    const b = document.getElementById(id) as HTMLButtonElement | null;
    if (!b) continue;
    b.classList.toggle('pending', busy);
    b.disabled = false;
  }
  setHint('mode-hint', busy ? t('mode_switching', { mode: want!.toUpperCase() }) : '');
}

export function setGraphMode(mode: string) {
  const target: 'std' | 'rta' | 'sdr' =
    mode === 'rta' ? 'rta' : mode === 'sdr' ? 'sdr' : 'std';
  if (pendingGraphMode() === null && target === currentGraphMode()) return;
  if (target !== 'std' && S.measOn) {
    exitMeasModePub(false);
    S.setMeasOn(false);
    const button = document.getElementById('btn-meas-onoff');
    if (button) button.textContent = t('off');
    setMeasButtons(false);
  }
  // A newer request replaces the previous one and restarts its timeout (discipline 2).
  requestGraphMode(target);
  syncModeButtons();
  // Sweep-to-SDR handoff: entering SDR from the swept view demodulates the frequency
  // the user located (active marker), or the current centre if no marker is set.
  if (target === 'sdr') {
    // Priority: an explicit hand-off (Shift+click / peak row already called sdrCenterHz.set), then
    // the user's own SDR tuning, then - first run only - the active marker or the swept centre.
    // Returning to SDR used to re-derive everything from the swept view, which discarded the
    // tuning and the listening setup the user had left there (reported).
    if (!sdrCenterHz.pending()) {
      const remembered = sdrCenterHz.confirmedValue();
      if (remembered !== null && remembered > 0) {
        sdrCenterHz.set(remembered);           // re-assert: the entry below sends it
      } else {
        const m = S.markers.find(x => x.enabled && x.freq != null);
        // Prefer the last centre confirmed by a SWP-family STATUS: centerHz.get() is refreshed
        // from every STATUS (including SDR ones), so it can still hold the value from before
        // a preset/re-tune when this runs.
        const base = swpCenterHz.get() > 0 ? swpCenterHz.get() : centerHz.get();
        sdrCenterHz.set((m && m.freq) ? m.freq : base);
      }
    }
    deferSdrAudioPreference();
  } else {
    sdrCenterHz.reset();
    if (sdrAudioHandoffTimer !== null) {
      window.clearTimeout(sdrAudioHandoffTimer);
      sdrAudioHandoffTimer = null;
    }
    setSdrAudioEnabled(false);
    setSdrIqEnabled(false);
  }
  send({ cmd: 'SET_MODE', mode: target });
}

/** Drop a pending request (socket loss / command error): the UI follows the backend again. */
export function releaseGraphModePending() {
  resetGraphMode();
  syncModeButtons();
}

export function syncGraphModeStatus(mode: string) {
  if (!isGraphMode(mode)) return;
  // Discipline 1: an older reply (a STATUS still on another mode) is not our answer.
  const changed = confirmGraphMode(mode);
  syncModeButtons();
  if (!changed) return;
  const isRtaLike = mode === 'rta' || mode === 'sdr';
  const isSdr = mode === 'sdr';
  S.setRtaMode(isRtaLike);
  S.setViewMode(isRtaLike ? 'rta' : 'std');
  S.setSdrMode(isSdr);
  if (isSdr) {
    // The IQ ingress runs while SDR mode is active, independent of the audio switch: the
    // backend only produces IQ during an SDR session, and the DSP input must not depend on
    // whether the speaker is muted.
    setSdrIqEnabled(true);
    deferSdrAudioPreference();
    // Entering SDR always fits the reference once: the swept level is meaningless for an IQS
    // panadapter (often 0 dBm against a -100 dBm floor), which is what produced the reported
    // "Preset -> SDR spectrum overflows the canvas". The backend needs a trace, so the client
    // asks for the fit as soon as frames arrive (same AUTO_SCALE command as the other modes).
    requestSdrEntryFit();
  } else {
    // Leaving SDR stops the audio PIPELINE; the user's preference is theirs and must survive the
    // trip. Writing it off here is what made "audio on" silently become "audio off" after a visit
    // to RTA/SWP (measured: localStorage flipped 1 -> 0 on the way out).
    setSdrAudioEnabled(false);
    setSdrIqEnabled(false);
    syncSdrAudioButton();
  }
  const modeButton = document.getElementById('btn-mode-rta');
  if (modeButton) modeButton.classList.toggle('active', mode === 'rta');
  const sdrButton = document.getElementById('btn-mode-sdr');
  if (sdrButton) sdrButton.classList.toggle('active', isSdr);
  localStorage.setItem('web-sa-mode', mode);
  if (isRtaLike) restoreRtaDensityCfg();

  document.body.classList.toggle('rta-mode', isRtaLike);
  const rtaDisable = [
    'select-smooth', 'select-refwin', 'btn-normalize', 'select-window',
    'select-detector',
    'input-points', 'btn-points',
  ];
  rtaDisable.forEach((id) => {
    const element = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
    if (element) element.disabled = isRtaLike;
  });
  // SDR-only: controls that do not apply to the IQ receive path (SWP RBW/VBW/sweep/spur
  // and the device trigger). Gain/amp, reference clock and the shared Ref stay enabled.
  const setDisabled = (id: string, off: boolean) => {
    const el = document.getElementById(id) as HTMLElement | null;
    if (!el) return;
    if (el instanceof HTMLInputElement || el instanceof HTMLSelectElement
        || el instanceof HTMLButtonElement) {
      el.disabled = off;
    }
    el.querySelectorAll('input,select,button').forEach((c) => {
      (c as HTMLInputElement).disabled = off;
    });
  };
  ['select-rbw-mode', 'input-rbw', 'unit-rbw-group',
    'select-vbw-mode', 'input-vbw', 'unit-vbw-group', 'btn-vbw-set',
    'select-sweep-mode', 'sweep-panel',
    'select-spur', 'btn-gapfill', 'trigger-panel',
  ].forEach((id) => setDisabled(id, isSdr));
  const resetButton = document.querySelector(
    '[data-action="reset-norm"]') as HTMLButtonElement | null;
  if (resetButton) resetButton.disabled = isRtaLike;
  const rtaFrequency = document.getElementById('rta-freq-settings');
  const swpFrequency = document.getElementById('swp-freq-settings');
  const sdrSettings = document.getElementById('sdr-settings');
  if (rtaFrequency) rtaFrequency.style.display = mode === 'rta' ? '' : 'none';
  if (swpFrequency) swpFrequency.style.display = mode === 'std' ? '' : 'none';
  if (sdrSettings) sdrSettings.style.display = isSdr ? '' : 'none';
  if (isRtaLike) S.resetWaterfall();
  if (isSdr && sdrCenterHz.pending() && sdrCenterHz.get() > 0) {
    const f = sdrCenterHz.get();
    if (!hasStoredSdrPrefs()) {
      // First run (or right after a Preset): pick a sensible listening setup for the band the
      // user landed in. Once they have their own preferences, those win - the band presets
      // (FM/AIR/VHF/UHF) are the deliberate way to switch bands.
      const inFm = f >= 87.5e6 && f <= 108e6;
      const inAir = f >= 118e6 && f <= 137e6;
      sdrDecimate.set(16);
      sdrDemod.set(inFm ? 'wfm' : 'am');
      sdrIfbw.set(inFm ? 180000 : (inAir ? 25000 : 12000));
      sdrDeemph.set(-1);
    }
    const decimate = sdrDecimate.get() || 16;
    sdrSpanHz.set(estimatedCaptureSpanHz(decimate));  // estimate until the device reports it
    sdrListenHz.set(sdrListenHz.get() > 0 ? sdrListenHz.get() : f);
    renderSdrState();
    send({ cmd: 'SET_SDR', center: f, decimate });
    send({ cmd: 'SET_SDR_TUNE', listen: sdrListenHz.get() });
    applySdrDemod();
  }
  requestRender();
}

// Density persistence/grain restored on page load + RTA entry (independent memory keys)
// ── SDR mode controls ──

function sdrNumber(id: string, fallback: number): number {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  const v = el ? parseFloat(el.value) : NaN;
  return isFinite(v) ? v : fallback;
}

export function applySdr() {
  const centerMhz = sdrNumber('input-sdr-center', 1000);
  const decimate = Math.round(sdrNumber('select-sdr-decimate', 32));
  const center = centerMhz * 1e6;
  // Record what the user typed as the intent; the STATUS confirms it later.
  sdrCenterHz.set(center);
  sdrDecimate.set(decimate);
  sdrListenHz.set(center);
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR', center, decimate });
  // Setting the wideband centre also tunes the demodulator there.
  send({ cmd: 'SET_SDR_TUNE', listen: center });
  // Apply the same band -> demod rule as the SWP/RTA handoff, otherwise entering SDR
  // directly and typing a broadcast frequency keeps the previous demod (e.g. AM on an FM
  // station = noise).
  const inFm = center >= 87.5e6 && center <= 108e6;
  const inAir = center >= 118e6 && center <= 137e6;
  if (inFm || inAir) {
    sdrDemod.set(inFm ? 'wfm' : 'am');
    sdrIfbw.set(inFm ? 180000 : 25000);
    renderSdrState();
    applySdrDemod();
  }
  const l = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (l) l.value = centerMhz.toFixed(6);
}

// Changing only the capture bandwidth keeps the listen frequency unchanged.
export function applySdrBw() {
  const decimate = Math.round(sdrNumber('select-sdr-decimate', 32));
  sdrDecimate.set(decimate);
  const center = sdrCenterHz.get();
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR', center, decimate });
}

export function applySdrTune() {
  const listenMhz = sdrNumber('input-sdr-listen', 1000);
  const f = listenMhz * 1e6;
  sdrListenHz.set(f);
  renderSdrState();
  prepareSdrAudioTransition();
  send({ cmd: 'SET_SDR_TUNE', listen: f });
}

export function applySdrDemod() {
  const mode = sdrDemod.get();
  // The browser DSP is told now, not at the next STATUS: the demodulator is what the listener is
  // changing, and waiting up to a second for it felt like the button had not worked.
  pushSdrPipeline();
  const ifbw = sdrIfbw.get();
  const deemph = sdrDeemph.get();
  const volume = sdrVolume.get();
  const squelch = sdrSquelch.get();
  // Only a demod-mode / IF-bandwidth / de-emphasis change rebuilds the chain and needs the
  // reset handshake. Volume/squelch/AGC are applied live, so muting them would just add a
  // gap. "Changed" is the slot's own pending state now, not a separate copy of the last
  // value sent (those caches were one more thing that could disagree with the truth).
  if (sdrDemod.pending() || sdrIfbw.pending() || sdrDeemph.pending()) {
    prepareSdrAudioTransition();
  }
  send({ cmd: 'SET_SDR_DEMOD', mode, ifbw, volume, squelch, agc: sdrAgc.get(),
         deemph_us: deemph });
}

/**
 * Noise reduction on/off. The browser runs the reducer, so this is a client-owned preference: the
 * worker is told directly and the backend is not involved (its Python audio path is only the
 * fallback/reference now).
 */
export function toggleSdrNr() {
  const on = !sdrNr.get();
  sdrNr.set(on);
  setSdrPipelineNr(on, sdrNrStrength.get());
  renderSdrState();
}

/** How hard the reducer pushes (0..1), applied live. */
export function setSdrNrStrength(strength: number) {
  if (!Number.isFinite(strength)) return;
  sdrNrStrength.set(strength);
  if (sdrNr.get()) setSdrPipelineNr(true, sdrNrStrength.get());
  renderSdrState();
}

function toggleSdrAgc(el: HTMLElement) {
  const on = !sdrAgc.get();
  sdrAgc.set(on);
  el.classList.toggle('active', on);
  el.textContent = on ? t('on') : t('off');
  // AGC is applied live on the backend; no chain rebuild, so no mute/reset.
  send({ cmd: 'SET_SDR_DEMOD', agc: on });
}

// Band presets (centre, capture bandwidth, demod, IF bandwidth)
const SDR_BANDS: Record<string, { center: number; decimate: number; demod: string; ifbw: number }> = {
  fm: { center: 98e6, decimate: 2, demod: 'wfm', ifbw: 180000 },
  air: { center: 127.5e6, decimate: 16, demod: 'am', ifbw: 25000 },
  vhf: { center: 145e6, decimate: 16, demod: 'nfm', ifbw: 12000 },
  uhf: { center: 435e6, decimate: 16, demod: 'nfm', ifbw: 12000 },
};

export function applySdrBand(name: string) {
  const b = SDR_BANDS[name];
  if (!b) return;
  prepareSdrAudioTransition();
  sdrCenterHz.set(b.center);
  sdrDecimate.set(b.decimate);
  sdrSpanHz.set(estimatedCaptureSpanHz(b.decimate)); // estimate until the device reports the real span
  sdrDemod.set(b.demod);
  sdrIfbw.set(b.ifbw);
  sdrListenHz.set(b.center);
  renderSdrState();
  send({ cmd: 'SET_SDR', center: b.center, decimate: b.decimate });
  send({ cmd: 'SET_SDR_TUNE', listen: b.center });
  applySdrDemod();
}

export function listenAtFreq(hz: number) {
  if (!isFinite(hz) || hz <= 0) return;
  if (currentGraphMode() === 'sdr') {
    sdrListenHz.set(hz);
    renderSdrState();
    prepareSdrAudioTransition();
    send({ cmd: 'SET_SDR_TUNE', listen: hz });
    requestRender();
    return;
  }
  // From the swept view: hand this frequency to SDR for demodulation. BOTH the capture centre and
  // the listen frequency go there - setting only the centre left the previous listen frequency in
  // place, so the backend re-centred the capture to chase a channel the user never asked for
  // (measured: Shift+click at 216 MHz landed SDR at 987 MHz).
  sdrCenterHz.set(hz);
  sdrListenHz.set(hz);
  setGraphMode('sdr');
}

function syncSdrButtons() {
  const mode = sdrDemod.get();
  document.querySelectorAll('[data-sdr-demod]').forEach((el) => {
    el.classList.toggle('active', (el as HTMLElement).dataset.sdrDemod === mode);
  });
  const ibw = Math.round(sdrIfbw.get());
  document.querySelectorAll('[data-sdr-ifbw]').forEach((el) => {
    el.classList.toggle('active', Number((el as HTMLElement).dataset.sdrIfbw) === ibw);
  });
  const dv = sdrDeemph.get();
  document.querySelectorAll('[data-sdr-deemph]').forEach((el) => {
    el.classList.toggle('active', Number((el as HTMLElement).dataset.sdrDeemph) === dv);
  });
}

// ── SDR audio enable + amplitude reference ──

/** The last channelizer facts from STATUS (`sdr.actual`): the DSP worker needs the rate the
 * backend's DDC produces, and only the device knows it. */
let sdrActual: Record<string, unknown> = {};

/**
 * Push the current SDR settings to the browser DSP.
 *
 * Called from the STATUS handler *and* from every user action that changes one of them: the panel
 * used to wait for the 1 Hz STATUS round trip before the worker learned about a new demodulator, so
 * the spectrum reacted at once while the audio followed a second later (reported: the demod and
 * audio controls felt unresponsive). The channelizer's rate still comes from STATUS because the
 * device owns it.
 */
function pushSdrPipeline(): void {
  const basebandRate = Number(sdrActual.ddc_rate) || 0;
  if (basebandRate <= 0) return;
  configureSdrPipeline(
    {
      fsIn: basebandRate,
      // The worklet plays the PCM as-is, so the DSP has to produce the device's rate (44.1 kHz
      // on many systems).
      outRate: audioSampleRate(),
      // The *UI's* selection drives the DSP worker, not the backend's demod: a digital mode
      // (ft8) has no backend DSP at all, and the Python fallback keeps its own demod anyway.
      mode: String(sdrDemod.get() || 'am'),
      ifBw: Number(sdrIfbw.get()) || 6000,
      pitch: Number(sdrPitchHz()) || 700,
      // -1 = the mode's default: the DSP resolves it against its own mode table.
      deemphUs: sdrDeemph.get(),
    },
    {
      volume: sdrVolume.get(),
      audioEnabled: true,
      nr: sdrNr.get(),
      nrStrength: sdrNrStrength.get(),
      squelch: sdrSquelch.get(),
    },
  );
}

/** The CW sidetone the backend confirmed (the panel has no control for it yet). */
function sdrPitchHz(): number {
  return Number(sdrActual.pitch) || 0;
}

function syncSdrAudioButton() {
  const b = document.getElementById('btn-sdr-audio');
  if (b) {
    b.textContent = sdrAudioOn.get() ? t('on') : t('off');
    b.classList.toggle('active', sdrAudioOn.get());
  }
}

function applySdrAudioPreference() {
  const on = sdrAudioOn.get();
  sdrAudioOn.set(on);
  setSdrAudioEnabled(on);
  setSdrDspAudioEnabled(on);
  syncSdrAudioButton();
}

export function toggleSdrAudio() {
  const on = !sdrAudioOn.get();
  sdrAudioOn.set(on); // the slot persists it (single writer)
  // Both audio owners are told: the Python worker (the fallback) and the browser DSP worker, which
  // is the one holding the worklet's port when it is the audio source.
  setSdrAudioEnabled(on);
  setSdrDspAudioEnabled(on);
  syncSdrAudioButton();
}

// SDR status -> panel readouts (called on every STATUS)
function sdrSet(id: string, value: string) {
  const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  if (el && document.activeElement !== el) el.value = value;
}

// Live SDR geometry for click/drag/wheel interaction (updated from STATUS).
export function syncSdrPanel(s: any) {
  const sdr = s?.sdr;
  if (!sdr) return;
  const a = sdr.actual || {};
  // Confirm the tuning group from the backend, then render it from the slots (the only
  // writer of those controls). The demod group still uses the DOM for now.
  sdrCenterHz.confirm(Number(sdr.center) || 0);
  sdrDecimate.confirm(Number(sdr.decimate) || 32);
  if (a.start != null && a.stop != null) sdrSpanHz.confirm(Number(a.stop) - Number(a.start));
  sdrListenHz.confirm(Number(sdr.listen) || 0);
  sdrDemod.confirm(String(sdr.demod || 'am'));
  sdrIfbw.confirm(Number(sdr.if_bw) || 6000);
  // The requested de-emphasis (-1 = per-mode default). Without this confirm the user's
  // choice stayed a pending intent and fell back to Auto when the slot TTL expired.
  if (sdr.deemph_us != null) sdrDeemph.confirm(Number(sdr.deemph_us));
  sdrVolume.confirm(Number(sdr.volume));
  sdrSquelch.confirm(Number(sdr.squelch));
  sdrAgc.confirm(!!sdr.agc);
  renderSdrState();
  // The demod group's controls are projections of the slots (the slots own the values, so a
  // preference restored from storage cannot disagree with the form).
  sdrSet('input-sdr-volume', String(sdrVolume.get()));
  sdrSet('input-sdr-squelch', String(Math.round(sdrSquelch.get())));
  const agc = document.getElementById('btn-sdr-agc');
  if (agc) {
    agc.textContent = sdrAgc.get() ? t('on') : t('off');
    agc.classList.toggle('active', sdrAgc.get());
  }
  const lvl = document.getElementById('cur-sdr-level');
  if (lvl) {
    // The browser DSP measures the PCM it produces; when it owns playback the backend's Python
    // demodulator is not running, so its level is not the one on screen.
    const level = dspLevelDbfs();
    const value = Number.isFinite(level) ? level : Number(sdr.level_dbfs);
    lvl.textContent = Number.isFinite(value) ? value.toFixed(1) + ' dBFS' : '';
  }
  syncSdrButtons();
  syncSdrAudioButton();
  syncSdrRefUI();
  // The demodulator parameters were confirmed by this STATUS: hand them to the browser DSP (the
  // user actions push them immediately as well, this keeps the two in step after a reconnect).
  sdrActual = (sdr.actual || {}) as Record<string, unknown>;
  pushSdrPipeline();
  // The FT8 table only means something while FT8 is the demodulator (the registry's id, not a label).
  setFt8WindowAvailable(String(sdr.demod || '') === 'ft8');
}
// 仅 ×N(6)/Manual(7) 需要输入框+Set 按钮; 其余固定档隐藏
// Turn all markers on/off at once (toggle)
// Preset
export function presetAll() {
  exitMeasModePub();
  S.setMeasOn(false);
  const b = document.getElementById('btn-meas-onoff');
  if (b) b.textContent = t('off');
  setMeasButtons(false);
  setDisplayRef('preset', 0);
  displayOffset.set(0);
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of) of.value = '0';
  S.traces.forEach((t, i) => { t.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; t.reference = null; t.isNormalized = false; t.avgSum = null; t.avgCount = 0; });
  S.markers.forEach(m => { m.enabled = false; m.mode = 'OFF'; m.tracking = false; });
  S.setM3dB(null); S.setAmpRes(null); S.setHarm(null); S.setPnmData(null);
  peakListVisible.set(false); S.setPeakMarks(null);
  resetPeakThr();
  const pl = document.getElementById('btn-peaklist');
  if (pl) pl.textContent = t('off');
  S.setActiveMkrId(1);
  syncMarkerTrackingToggle();
  // Preset also resets the RTA session (backend reset_defaults) and clears the RTA
  // memory + UI so re-entering RTA starts from factory defaults.
  if (S.rtaMode) {
    const spanSel = document.getElementById('select-rta-span') as HTMLSelectElement | null;
    if (spanSel) spanSel.value = '50781250';
    const rbwSel = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
    if (rbwSel) rbwSel.value = 'auto';
    const vbwSel = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
    if (vbwSel) vbwSel.value = 'equal';
    const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement;
    if (sm) { sm.value = '2'; syncSweepInput(); }
    clearRtaAccum();
  }
  // Preset must also clear the browser-side records, otherwise a reload restores the old
  // mode / audio / ref / RTA-fade / limit-line state instead of the power-on defaults.
  try {
    ['web-sa-mode', 'web-sa-sdr-audio', 'web-sa-sdr-ref-auto',
     'rta-fade', 'rta-bins'].forEach((k) => localStorage.removeItem(k));
  } catch { /* ignore */ }
  resetLimits();
  S.resetWaterfall();
  wfPaused.set(false);
  smoothBins.set(1);
  spanStepAuto.set(true);
  // Preset resets the device, not the listener: the audio switch and the IQ ingress survive. The
  // backend's reconfigure re-anchors the stream with its own flush frame, so stopping the ingress
  // here only orphaned it (re-enabling audio afterwards still heard nothing until a reload).
  const audioWasOn = sdrAudioOn.get();
  resetSdrIq();

  // Every pending SDR intent (including a hand-off centre) is dropped by one call - the old
  // code cleared the fields by hand and missed one, so a Preset could reapply the previous
  // frequency.
  resetSdrState();
  sdrAudioOn.set(audioWasOn);
  renderSdrState();
  resetAutoScaleState();               // the reference is about to be reset: forget the last fit
  send({ cmd: 'SET_PRESET' });
  setSdrDspAudioEnabled(audioWasOn);
  setSdrAudioEnabled(audioWasOn);
  syncSdrAudioButton();
  updateInfoBar(); applyMeasUI(); requestRender();
}

// Current frontend time (shown when not locked)
// GNSS detail popover: fill + show/close
// Sync all toggle button texts when the language changes
// ── Panel collapse ──
// ── data-action binding ──
export function bindActions() {
  const act: Record<string, (el: HTMLElement) => void> = {
    'apply-center-span': () => applyCenterSpan(),
    'apply-start-stop': () => applyStartStop(),
    'full-span': () => applyFullSpan(),
    'swp-span-down': () => stepSwpSpan(-1),
    'swp-span-up': () => stepSwpSpan(1),
    'span-step-auto': () => resetSpanStepAuto(),
    'set-ref-level': () => setRefLevel(),
    'set-ref-auto': () => setRefAuto(),
    'ref-down': () => adjustRefLevel(-1),
    'ref-up': () => adjustRefLevel(1),
    'apply-rbw': () => applyRBW(),
    'apply-vbw': () => applyVBW(),
    'apply-points': () => applyPoints(),
    'set-window': (el) => setWindow((el as HTMLSelectElement).value),
    'set-spur': (el) => setSpurMode((el as HTMLSelectElement).value),
    'set-detector': (el) => send({ cmd: 'SET_DETECTOR', mode: (el as HTMLSelectElement).value }),
    'set-refclock': (el) => setRefClock((el as HTMLSelectElement).value),
    'toggle-refclkout': () => toggleRefClkOut(),
    'set-amp': () => setAmp(),
    'set-trace-mode': (el) => setTraceMode((el as HTMLSelectElement).value),
    'set-trace-avg': (el) => setTraceAverage(parseInt((el as HTMLSelectElement).value) || 0),
    'export-csv': () => exportActiveTraceCsv(),
    'export-png': () => exportSpectrumPng(),
    'export-peaks-csv': () => exportPeakListCsv(),
    'set-smooth': (el) => { smoothBins.set(parseInt((el as HTMLSelectElement).value) || 1); requestRender(); },
    'set-norm-refwin': (el) => {
      const v = parseInt((el as HTMLSelectElement).value) || 0;
      setNormRefWinUser(v);
      const t = S.traces[S.activeTraceIdx];
      if (t.reference && t.isNormalized && t.powers) {
        const ref = buildReferenceTablePub(t.powers);
        t.reference = smoothRefWindow(ref, normRefWindow());
      }
      requestRender();
    },
    'toggle-freeze': () => toggleFreeze(),
    'normalize': () => normalizeActiveTrace(),
    'reset-norm': () => resetActiveTraceNormalize(),
    'clear-trace': () => {
      if (S.rtaMode) { clearRtaTrace(); }
      else resetTraceAccum(S.traces[S.activeTraceIdx]);
    },
    'mkr-peak': () => activeMarkerPeak(),
    'mkr-peak-left': () => activeMarkerNextPeakLeft(),
    'mkr-peak-right': () => activeMarkerNextPeakRight(),
    'mkr-valley': () => activeMarkerValley(),
    'mkr-valley-left': () => activeMarkerNextValleyLeft(),
    'mkr-valley-right': () => activeMarkerNextValleyRight(),
    'mkr-center': () => markerToCenter(),
    'peakthr-auto': () => peakThrAuto(),
    'toggle-peaklist': () => togglePeakList(),
    'meas-toggle': () => measToggle(),
    'meas-amp': () => measureAmp(),
    'clear-amp': () => clearAmp(),
    'meas-chan': () => measureChannel(),
    'clear-chan': () => clearChannel(),
    'meas-harm': () => measHarmApply(),
    'meas-pnm': () => measPnmApply(),
    'connect': () => connectDevice(),
    'gnss-detail': () => fillGnssDetail(),
    'gnss-close': () => closeGnssDetail(),
    'refclk-detail': () => openRefClockDetail(),
    'refclk-close': () => closeRefClockDetail(),
    'markers-all': () => toggleMarkersAll(),
    'marker-tracking': () => toggleActiveMarkerTracking(),
    'set-sweep': () => { syncSweepInput(); setSweepSpeed(); },
    'toggle-rta': () => setGraphMode(currentGraphMode() === 'rta' ? 'swp' : 'rta'),
    'apply-rta': () => applyRta(),
    'toggle-sdr': () => setGraphMode(currentGraphMode() === 'sdr' ? 'swp' : 'sdr'),
    'apply-sdr': () => applySdr(),
    'apply-sdr-bw': () => applySdrBw(),
    'apply-sdr-tune': () => applySdrTune(),
    'set-sdr-demod': () => applySdrDemod(),
    'toggle-sdr-agc': (el) => toggleSdrAgc(el),
    'toggle-sdr-nr': () => toggleSdrNr(),
    'toggle-ft8-window': () => toggleFt8Window(),
    'set-sdr-nr-strength': (el) => setSdrNrStrength(Number((el as HTMLSelectElement).value)),
    'toggle-sdr-audio': () => toggleSdrAudio(),
    'rta-span-down': () => rtaSpanStep(1),
    'rta-span-up': () => rtaSpanStep(-1),
    'rta-span-full': () => rtaSpanFull(),
    'set-rta-fade': (el) => { rtaFade.set(parseFloat((el as HTMLSelectElement).value) || 0.98); try { localStorage.setItem('rta-fade', (el as HTMLSelectElement).value); } catch {} },
    'set-rta-bins': (el) => { setRtaBins(parseInt((el as HTMLSelectElement).value) || 128); },
    'wf-pause': () => toggleWfPause(),
    'wf-reset': () => resetWf(),
    'preset': () => presetAll(),
    'toggle-gapfill': () => toggleGapFill(),
    'toggle-group': (el) => toggleGroup(el),
    'toggle-all-groups': () => toggleAllGroups(),
  };
  document.querySelectorAll('[data-action]').forEach(el => {
    const a = el.getAttribute('data-action')!;
    const handler = act[a];
    if (!handler) return;
    if (el.tagName === 'SELECT') {
      (el as HTMLSelectElement).addEventListener('change', () => handler(el as HTMLElement));
    } else {
      el.addEventListener('click', () => handler(el as HTMLElement));
    }
  });
  document.addEventListener('websa:unit-commit', (event) => {
    const detail = (event as CustomEvent<{ field: string; commit: boolean }>).detail;
    commitUnitField(detail.field, detail.commit);
  });

  const selectOnFocus = [
    'input-center', 'input-span', 'input-start', 'input-stop', 'input-rta-center',
    'input-rbw', 'input-vbw', 'input-pnm', 'input-span-step',
    'input-sdr-center', 'input-sdr-listen',
  ];
  for (const id of selectOnFocus) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    input?.addEventListener('focus', () => requestAnimationFrame(() => input.select()));
  }

  // SDR demod panel: ranges commit on change (not on every drag pixel), and the
  // frequency inputs commit on Enter.
  const volumeEl = document.getElementById('input-sdr-volume') as HTMLInputElement | null;
  volumeEl?.addEventListener('change', () => {
    // A slider at its left end means 0, which is silence - `x || 0.8` read that 0 as "unset" and put
    // the volume back to 0.8, so the listener heard the noise floor at nearly full level, louder than
    // at the settings just beside it, and no amount of turning the level down reached silence.
    const wanted = Number.parseFloat(volumeEl.value);
    sdrVolume.set(Number.isFinite(wanted) ? Math.max(0, Math.min(2, wanted)) : 0.8);
    pushSdrPipeline();
    applySdrDemod();
  });
  const squelchEl = document.getElementById('input-sdr-squelch') as HTMLInputElement | null;
  squelchEl?.addEventListener('change', () => {
    sdrSquelch.set(parseFloat(squelchEl.value) || -110);
    setSdrPipelineSquelch(sdrSquelch.get());
    applySdrDemod();
  });
  // The demod group is generated from the DSP plugin registry, so the mode buttons and the
  // kernels cannot disagree; the handler is the same path the panel always used.
  // The FT8 decode window: its toggle lives next to the readout, its rows come from the log, and a
  // row click tunes the receiver (the only action an FT8 operator takes on a decode).
  // A row tunes the *dial* so the signal lands inside the decoder's band (see `ft8DialFor`).
  initFt8Window({ onTune: (hz) => listenAtFreq(ft8DialFor(hz)) });
  const demodGroup = document.getElementById('sdr-demod-group');
  if (demodGroup) {
    void initSdrDemodGroup(demodGroup, {
      onSelect: (id) => {
        sdrDemod.set(id);
        renderSdrState();
        setFt8WindowAvailable(id === 'ft8');
        applySdrDemod();
      },
    });
  }
  document.querySelectorAll('[data-sdr-ifbw]').forEach((el) => {
    el.addEventListener('click', () => {
      sdrIfbw.set(Number((el as HTMLElement).dataset.sdrIfbw) || 6000);
      renderSdrState();
      pushSdrPipeline();
      applySdrDemod();
    });
  });
  document.querySelectorAll('[data-sdr-deemph]').forEach((el) => {
    el.addEventListener('click', () => {
      const value = Number((el as HTMLElement).dataset.sdrDeemph ?? -1);
      sdrDeemph.set(value);
      // The browser runs the audio chain, so it gets the change now (the backend's command only
      // records it for the fallback and, with the browser demodulating, must not reconfigure).
      setSdrPipelineDeemph(value);
      renderSdrState();
      applySdrDemod();
    });
  });
  document.querySelectorAll('[data-sdr-band]').forEach((el) => {
    el.addEventListener('click', () => applySdrBand((el as HTMLElement).dataset.sdrBand || 'fm'));
  });
  const sdrListenEl = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (sdrListenEl) sdrListenEl.addEventListener('change', () => applySdrTune());
  const sdrCenterEl = document.getElementById('input-sdr-center') as HTMLInputElement | null;
  if (sdrCenterEl) sdrCenterEl.addEventListener('change', () => applySdr());
  // Persisted SDR preferences are restored by the slots themselves (core/params.ts).
  syncSdrAudioButton();
  syncSdrRefUI();
  const sdrCenter = document.getElementById('input-sdr-center') as HTMLInputElement | null;
  if (sdrCenter) sdrCenter.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdr();
  });
  const sdrListen = document.getElementById('input-sdr-listen') as HTMLInputElement | null;
  if (sdrListen) sdrListen.addEventListener('keydown', (ev) => {
    if ((ev as KeyboardEvent).key === 'Enter') applySdrTune();
  });

  // Special bindings: trace tab / meas tab / marker select / scale / peakthr input
  document.querySelectorAll('[data-trace-tab]').forEach(el => {
    el.addEventListener('click', () => switchTraceTab(parseInt((el as HTMLElement).dataset.traceTab || '0')));
  });
  document.querySelectorAll('[data-meas-tab]').forEach(el => {
    el.addEventListener('click', () => measTab((el as HTMLElement).dataset.measTab || 'amp'));
  });

  const wfBtn = document.getElementById('btn-waterfall');
  if (wfBtn) wfBtn.addEventListener('click', () => toggleWaterfall());
  document.querySelectorAll('[data-marker-select]').forEach(el => {
    el.addEventListener('click', () => selectMarker(parseInt((el as HTMLElement).dataset.markerSelect || '1')));
  });
  // The marker table's active-arrow dispatches this so the table can switch the active
  // marker without importing controls (and syncs the right-hand Marker buttons).
  document.addEventListener('websa:marker-active', (event) => {
    const id = (event as CustomEvent<{ id: number }>).detail?.id;
    if (id) selectMarker(Number(id));
  });
  document.querySelectorAll('[data-scale]').forEach(el => {
    el.addEventListener('click', () => setScale(parseFloat((el as HTMLElement).dataset.scale || '10')));
  });
  const sm = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  if (sm) { sm.addEventListener('change', () => syncSweepInput()); syncSweepInput(); }
  const pt = document.getElementById('input-peakthr') as HTMLInputElement;
  if (pt) {
    pt.addEventListener('input', () => peakThrManual());
    pt.addEventListener('change', () => peakThrManual());
  }
  const off = document.getElementById('input-offset') as HTMLInputElement;
  if (off) off.addEventListener('change', () => setOffset());
  ['center', 'span', 'start', 'stop'].forEach(f => {
    const el = document.getElementById(`input-${f}`) as HTMLInputElement;
    if (!el) return;
    el.addEventListener('input', () => {
      el.dataset.edited = '1';
      markFrequencyDirty('swp-freq-settings');
    });
    el.addEventListener('keydown', (event) => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      if (f === 'center' || f === 'span') applyCenterSpan();
      else applyStartStop();
    });
  });
  const rtaCenter = document.getElementById('input-rta-center') as HTMLInputElement | null;
  if (rtaCenter) {
    rtaCenter.addEventListener('input', () => {
      rtaCenter.dataset.edited = '1';
      markFrequencyDirty('rta-freq-settings');
    });
    rtaCenter.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') { event.preventDefault(); applyRta(); }
    });
  }
  for (const id of ['input-rbw', 'input-vbw', 'input-pnm']) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    input?.addEventListener('input', () => { input.dataset.edited = '1'; });
  }
  const spanStep = document.getElementById('input-span-step') as HTMLInputElement | null;
  if (spanStep) {
    spanStep.addEventListener('input', () => updateCustomSpanStep());
    spanStep.addEventListener('change', () => updateCustomSpanStep());
  }
  syncSwpSpanStep();
  const refInput = document.getElementById('input-ref') as HTMLInputElement | null;
  if (refInput) refInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') { event.preventDefault(); setRefLevel(); }
  });
  const hf = document.getElementById('input-harm-f0') as HTMLInputElement;
  if (hf) hf.addEventListener('input', () => autoHarmSpan());
}

// Canvas click/drag
// ── SDR canvas interaction: click to tune, drag to tune/pan ──
let sdrDown = false;
let sdrMoved = false;
let sdrX0 = 0;
let sdrEdgeAt = 0;

function canvasX(e: MouseEvent, canvas: HTMLCanvasElement): number {
  // The drawing space is the canvas content box (core/store.ts W). Under a UI scale the
  // element is CSS-zoomed, so clientWidth is in *local* px while the pointer and the rect are
  // in screen px: convert with the zoomed width (rect includes the 1px border on each side).
  const rect = canvas.getBoundingClientRect();
  const scale = getUiScale();
  const border = (parseFloat(getComputedStyle(canvas).borderLeftWidth) || 0) * scale;
  const box = Math.max(1, rect.width - 2 * border);
  return (e.clientX - rect.left - border) * (S.W / box);
}

function xToFreqHz(x: number): number | null {
  const f = S.freqArray;
  if (!f || f.length < 2) return null;
  const pr = plotRectPub();
  const t = (x - pr.x) / pr.w;
  if (t < 0 || t > 1) return null;
  const idx = t * (f.length - 1);
  const i0 = Math.max(0, Math.min(f.length - 2, Math.floor(idx)));
  const fr = idx - i0;
  return f[i0] * (1 - fr) + f[i0 + 1] * fr;
}

export function bindCanvas() {
  const canvas = document.getElementById('spectrum') as HTMLCanvasElement;
  if (!canvas) return;

  canvas.addEventListener('mousedown', (e) => {
    const pr = plotRectPub();
    const x = canvasX(e, canvas);
    if (currentGraphMode() === 'sdr') {
      if (x < pr.x || x > pr.x + pr.w) return;
      sdrDown = true; sdrMoved = false; sdrX0 = x;
      S.setDragging(true);
      return;
    }
    const p = getDisplayPowers();
    if (!p) return;
    if (x < pr.x || x > pr.x + pr.w) return;
    if (e.shiftKey) {
      // Shift+click: jump straight to SDR demodulation at this frequency.
      const f = xToFreqHz(x);
      if (f != null) listenAtFreq(f);
      return;
    }
    S.setDragging(true);
    placeMarkerFromX(x, p);
  });

  window.addEventListener('mousemove', (e) => {
    if (!S.dragging) return;
    const x = canvasX(e, canvas);
    if (sdrDown) {
      if (Math.abs(x - sdrX0) > 4) sdrMoved = true;
      if (sdrMoved) {
        // Preview the marker locally; the tune itself is committed on release.
        const f = xToFreqHz(x);
        if (f != null) sdrListenHz.set(f);
        // Edge push: dragging past the sides shifts the capture window so the user can
        // walk through adjacent frequency ranges (standard SDR panning). Throttled
        // because it retunes the device.
        const pr = plotRectPub();
        const frac = (x - pr.x) / pr.w;
        const now = performance.now();
        if (now - sdrEdgeAt > 350) {
          if (frac > 0.9) {
            sdrEdgeAt = now;
            sdrCenterHz.set(sdrCenterHz.get() + sdrSpanHz.get() * 0.2);
            sdrX0 += pr.w * 0.2;
            send({ cmd: 'SET_SDR', center: sdrCenterHz, decimate: sdrDecimate });
          } else if (frac < 0.1) {
            sdrEdgeAt = now;
            sdrCenterHz.set(sdrCenterHz.get() - sdrSpanHz.get() * 0.2);
            sdrX0 -= pr.w * 0.2;
            send({ cmd: 'SET_SDR', center: sdrCenterHz, decimate: sdrDecimate });
          }
        }
      }
      return;
    }
    const p = getDisplayPowers();
    if (!p) return;
    placeMarkerFromX(x, p);
  });

  window.addEventListener('mouseup', (e) => {
    if (sdrDown) {
      // Commit the tune once, on release (click or drag).
      const raw = xToFreqHz(canvasX(e, canvas));
      const f = raw;
      if (f != null) {
        sdrListenHz.set(f);
        send({ cmd: 'SET_SDR_TUNE', listen: f });
        requestRender();
      }
      sdrDown = false;
      sdrMoved = false;
      S.setDragging(false);
      return;
    }
    S.setDragging(false);
  });

  // Keyboard / trackpad-only operation (no mouse required).
  document.addEventListener('keydown', (e) => {
    if (currentGraphMode() !== 'sdr') return;
    const el = document.activeElement as HTMLElement | null;
    const tag = el?.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
    const mult = e.shiftKey ? 100 : (e.altKey ? 10 : 1);
    let handled = true;
    if (e.key === 'ArrowLeft') sdrTuneBy(-1000 * mult);
    else if (e.key === 'ArrowRight') sdrTuneBy(1000 * mult);
    else if (e.key === 'ArrowUp') sdrNudgeVolume(0.05);
    else if (e.key === 'ArrowDown') sdrNudgeVolume(-0.05);
    else if (e.key === 'PageUp') sdrCycleIfbw(1);
    else if (e.key === 'PageDown') sdrCycleIfbw(-1);
    else if (e.key === 'm' || e.key === 'M') sdrCycleDemod();
    else if (e.key === ' ') toggleSdrAudio();
    else handled = false;
    if (handled) e.preventDefault();
  });
}

// ── SDR keyboard helpers ──
const SDR_IFBW = [500, 2400, 3000, 6000, 12000, 25000, 50000, 100000, 180000];

function sdrTuneBy(dHz: number) {
  const center = sdrCenterHz.get();
  const span = sdrSpanHz.get();
  if (!(center > 0) || !(span > 0)) return;
  const f = Math.max(center - span / 2,
    Math.min(center + span / 2, (sdrListenHz.get() || center) + dHz));
  sdrListenHz.set(f);
  renderSdrState();
  send({ cmd: 'SET_SDR_TUNE', listen: f });
  requestRender();
}

function sdrCycleIfbw(dir: number) {
  const cur = sdrIfbw.get();
  let idx = SDR_IFBW.findIndex(v => v >= cur);
  if (idx < 0) idx = SDR_IFBW.length - 1;
  else if (SDR_IFBW[idx] > cur) idx = Math.max(0, idx - 1);
  const ni = Math.max(0, Math.min(SDR_IFBW.length - 1, idx + dir));
  sdrIfbw.set(SDR_IFBW[ni]);
  renderSdrState();
  applySdrDemod();
}

function sdrCycleDemod() {
  // The cycle is the registry's list (analog then digital), not a copy of it: a mode added to the
  // DSP appears here without another edit.
  const modes = sdrModeIds();
  if (modes.length === 0) return;                 // manifest not loaded yet: keep the current mode
  const current = sdrDemod.get();
  const index = modes.indexOf(current);
  sdrDemod.set(modes[(index + 1) % modes.length]);
  renderSdrState();
  applySdrDemod();
}

function sdrNudgeVolume(dv: number) {
  // `|| 0.8` here had the same defect as the slider's: at 0 the nudge started from 0.8 again.
  const from = Number.isFinite(sdrVolume.get()) ? sdrVolume.get() : 0.8;
  const next = Math.max(0, Math.min(2, from + dv));
  sdrVolume.set(next);
  const inp = document.getElementById('input-sdr-volume') as HTMLInputElement | null;
  if (inp) inp.value = String(next);
  applySdrDemod();
}
import { plotRect as plotRectPub } from '../render/plot';
import { exitMeasMode as exitMeasModePub } from './measure';
import { rtaFade, wfPaused } from './waterfallState';
import { displayOffset, smoothBins } from './displayState';
import { peakListVisible } from './measurePrefs';
import { spanStepAuto } from './swpState';

// ── Re-exports for the rest of the app ──
// The panel modules own these actions; the previous public surface (everything imported
// from ui/controls) is kept so no caller had to change (report finding P1-5).
export {
  applyCenterSpan, applyFullSpan, applyStartStop, markFrequencyDirty, resetSpanStepAuto,
  stepSwpSpan, syncFrequencyEditorStatus, syncSwpSpanStep, updateCustomSpanStep,
} from './panels/frequency';
export { applyPoints, applyRBW, applyVBW, setSpurMode, setWindow } from './panels/resolution';
export {
  applyRta, clearRtaAccum, restoreRtaDensityCfg, rtaSpanFull, rtaSpanStep, setRtaBins,
} from './panels/rta';
export {
  activeMarkerPeak, activeMarkerValley, autoTrackMarker, markerToCenter, placeMarkerFromX,
  selectMarker, syncMarkerTrackingToggle, toggleActiveMarkerTracking, toggleMarkersAll,
  updateMarkersAllBtn,
} from './panels/markers';
export {
  adjustRefLevel, refStepDbm, setAmp, setOffset, setRefAuto, setRefClock, setRefLevel,
  setScale, syncRefClkOut, syncScaleButtons, toggleGapFill, toggleRefClkOut,
} from './panels/refAmp';
export {
  resetWf, setSweepSpeed, syncSweepInput, toggleWaterfall, toggleWfPause,
} from './panels/waterfall';
export { closeGnssDetail, fillGnssDetail } from './panels/gnss';
export { syncToggleIcons, syncToggleTexts, toggleAllGroups, toggleGroup } from './panels/groups';
export { commitUnitField } from './panels/commit';
export { currentGraphMode };
