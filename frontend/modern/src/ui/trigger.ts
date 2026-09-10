// Trigger panel for the RTA acquisition trigger, with an on-demand capture workflow.
//
// Workflow contract (measured on the SAN-90):
//  - the device is in `bus` mode by default, so the display is live;
//  - pressing Capture arms a level trigger: the canvas is CLEARED first, because while the
//    trigger waits the device sends no packets at all and a leftover picture is
//    indistinguishable from a live one (that ambiguity is the whole reason for this flow);
//  - when the threshold is crossed the captured frame arrives and stays on screen
//    (TRIGGERED), and the button returns to its default label so the capture is explicit;
//  - Free Run (or Esc) always returns to the live view.
import * as S from '../core/store';
import { applyI18n, onLangChange, t } from '../core/i18n';
import { send } from '../core/wsSend';
import { renderAll } from '../render/spectrum';
import { getDisplayPowers } from '../dsp/peaks';
import { onTriggerHit, setArmedAt, setBackendWaiting } from './triggerEvents';
import { armSwpTrigger, disarmSwpTrigger, onSwpHit } from './swpTrigger';

const POLL_MS = 300;
const TICK_MS = 1000;

type Phase = 'free' | 'waiting' | 'hit';

let phase: Phase = 'free';
let since = 0;
let hitAt = '';
let hitLine = '';
let lastFrames = -1;
let armedConfirmed = false;
let armLevel = -40;
let armPeak: number | null = null;
let lastOverlayKey = '';
let lastArmAt = 0;

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function fmtClock(d: Date): string {
  const p2 = (n: number) => String(n).padStart(2, '0');
  return `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`;
}

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const p2 = (n: number) => String(n).padStart(2, '0');
  return s < 3600 ? `${p2(Math.floor(s / 60))}:${p2(s % 60)}` : `${Math.floor(s / 3600)}h${p2(Math.floor((s % 3600) / 60))}`;
}

function fmtTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '-';
  if (seconds < 1e-3) return `${Math.round(seconds * 1e6)} \u00b5s`;
  if (seconds < 1) return `${(seconds * 1e3).toFixed(1)} ms`;
  return `${seconds.toFixed(2)} s`;
}

function peakOfDisplay(): number | null {
  const d = getDisplayPowers();
  if (!d) return null;
  let peak = -Infinity;
  for (let i = 0; i < d.length; i++) {
    const v = d[i];
    if (Number.isFinite(v) && v > peak) peak = v;
  }
  return Number.isFinite(peak) ? peak : null;
}

/** Drop the current picture so "waiting" cannot be confused with live data. */
function clearDisplay(): void {
  S.traces.forEach((tr) => { tr.powers = null; tr.raw = null; });
  S.setRtaDisplays([null, null, null, null]);
  S.setRtaData(null);
  S.setRtaDensity2d(null);
  S.setPeakMarks(null);
  S.resetWaterfall();
}

function button(): HTMLButtonElement | null {
  return el<HTMLButtonElement>('btn-trg-capture');
}

function syncButton(): void {
  const b = button();
  if (!b) return;
  const key = phase === 'waiting' ? 'trg_btn_stop' : phase === 'hit' ? 'trg_btn_again' : 'trg_btn_capture';
  const label = t(key);
  if (b.textContent !== label) b.textContent = label;
  b.classList.toggle('active', phase === 'waiting');
  b.setAttribute('aria-pressed', String(phase === 'waiting'));
}

function syncOverlay(): void {
  const lines: string[] = [];
  if (phase === 'free') {
    lines.push(t('trg_chip_free'));
  } else if (phase === 'waiting') {
    lines.push(`${t('trg_chip_wait')} ${fmtElapsed(performance.now() - since)}`);
    if (armPeak !== null && armLevel > armPeak) {
      lines.push(`!${t('trg_never', { db: (armLevel - armPeak).toFixed(1) })}`);
    } else {
      lines.push(t(S.rtaMode ? 'trg_cross' : 'trg_swp_cross'));
    }
    if (armPeak !== null) lines.push(t('trg_peak', { v: armPeak.toFixed(1) }));
    if (S.trigPoi > 0) lines.push(t('trg_poi', { t: fmtTime(S.trigPoi) }));
  } else {
    lines.push(`${t('trg_chip_hit')} ${hitAt}`);
    if (hitLine) lines.push(hitLine);
    lines.push(t('trg_hit_hint'));
  }
  const key = lines.join('|');
  if (key !== lastOverlayKey) {
    lastOverlayKey = key;
    S.setTrigOverlay(lines);
  }
  syncButton();
}

async function pollOnce(): Promise<void> {
  try {
    const r = await fetch('/api/state', { cache: 'no-store' });
    const d = await r.json();
    const req = (d?.req?.rta ?? {}) as Record<string, any>;
    const status = (req.trigger_actual ?? {}) as Record<string, any>;
    S.setTrigPoi(Number((d?.rta_actual ?? {}).poi ?? 0));
    const source = String(req.trigger_source ?? 'bus');
    const armed = source !== 'bus' && source !== 'freerun';
    const frames = Number(status.frames ?? -1);
    const advanced = lastFrames >= 0 && frames > lastFrames;

    setBackendWaiting(armed && status.waiting === true);
    if (phase === 'waiting') {
      if (!armedConfirmed && armed && status.waiting === true) {
        armedConfirmed = true;            // device reports it is waiting -> baseline now
        lastFrames = frames;
      } else if (armedConfirmed && advanced) {
        phase = 'hit';
        hitAt = fmtClock(new Date());
        // The captured packet is already on screen; freeze it by leaving the data alone.
        S.setTrigWaiting(false);
        S.setTrigHit(true);
      }
    }
    lastFrames = frames;
    // The backend owns the armed state and it survives page reloads and mode switches, so
    // keep the two in sync: a fresh page (or another client) must not leave the display
    // waiting forever, and a stale local "waiting" must not survive a backend reset.
    const backendArmed = armed;
    // Both reconciliations below must stay quiet right after arming: the first poll can
    // race the command dispatch (it still reads 'bus'), and acting on that would undo the
    // arming that the user just asked for.
    const settling = performance.now() - lastArmAt < 2000;
    if (S.rtaMode && !settling && backendArmed && phase === 'free') {
      disarm();                            // armed elsewhere (reload / other client)
      return;
    }
    if (S.rtaMode && !settling && !backendArmed && phase !== 'free') {
      phase = 'free';                     // backend went back to free run (mode switch, reload)
      armedConfirmed = false;
      setBackendWaiting(false);
      S.setTrigArmed(false);
      S.setTrigWaiting(false);
      S.setTrigHit(false);
    }
    // thresholds / parameter echo while idle
    if (phase === 'free') {
      const lvl = el<HTMLInputElement>('input-trg-level');
      if (lvl && document.activeElement !== lvl && Number.isFinite(Number(req.trigger_level))) {
        lvl.value = String(Number(req.trigger_level));
      }
      armLevel = Number(req.trigger_level ?? armLevel);
    }
    S.setTrigLevel(armLevel);
    syncOverlay();
    renderAll();
  } catch {
    /* device offline: keep the last state */
  }
}

function schedule(): void {
  window.setTimeout(() => { void pollOnce().finally(schedule); }, phase === 'waiting' ? POLL_MS : 900);
}

/**
 * SWP software level trigger: the swept engine has no level trigger, so the same
 * threshold/edge condition is evaluated across consecutive sweeps instead (the display
 * keeps sweeping live until a crossing is found, then it freezes).
 */
function armSwept(): void {
  const input = el<HTMLInputElement>('input-trg-level');
  const level = parseFloat(input?.value ?? '');
  if (!Number.isFinite(level)) return;
  armLevel = level;
  armPeak = peakOfDisplay();
  S.setTrigLevel(level);
  S.setSwpEdge(edgeOf(selectValue('select-trg-edge')));
  armSwpTrigger();
  phase = 'waiting';
  since = performance.now();
  lastArmAt = since;
  syncOverlay();
  renderAll();
}

function selectValue(id: string): string {
  return el<HTMLSelectElement>(id)?.value ?? '';
}
function edgeOf(v: string): 'rising' | 'falling' | 'double' {
  return v === 'falling' ? 'falling' : v === 'double' ? 'double' : 'rising';
}

function arm(): void {
  const input = el<HTMLInputElement>('input-trg-level');
  const level = parseFloat(input?.value ?? '');
  if (!Number.isFinite(level)) return;
  armLevel = level;
  armPeak = peakOfDisplay();
  const sel = el<HTMLSelectElement>('select-trg-source');
  if (sel) sel.value = 'level';
  clearDisplay();                       // waiting must not look like live data
  phase = 'waiting';
  since = performance.now();
  armedConfirmed = false;
  S.setTrigArmed(true);
  S.setTrigWaiting(true);
  S.setTrigHit(false);
  lastArmAt = performance.now();
  setArmedAt(lastArmAt);
  setBackendWaiting(false);
  send({ cmd: 'SET_TRIGGER', source: 'level', level });
  syncOverlay();
  renderAll();
}

function disarm(): void {
  disarmSwpTrigger();
  const sel = el<HTMLSelectElement>('select-trg-source');
  if (sel) sel.value = 'bus';
  send({ cmd: 'SET_TRIGGER', source: 'bus' });
  phase = 'free';
  armedConfirmed = false;
  setBackendWaiting(false);
  S.setTrigArmed(false);
  S.setTrigWaiting(false);
  S.setTrigHit(false);
  syncOverlay();
  renderAll();
}

function push(payload: Record<string, unknown>): void {
  send({ cmd: 'SET_TRIGGER', ...payload });
}

function wireNumber(id: string, key: string): void {
  const i = el<HTMLInputElement>(id);
  i?.addEventListener('change', () => {
    const v = parseFloat(i.value);
    if (Number.isFinite(v)) push({ [key]: v });
  });
}

function wireSelect(id: string, key: string): void {
  const s = el<HTMLSelectElement>(id);
  s?.addEventListener('change', () => {
    if (key === 'source' && s.value === 'bus') { disarm(); return; }
    push({ [key]: s.value });
  });
}

export function initTrigger(): void {
  const panel = document.getElementById('trigger-panel');
  if (panel) applyI18n(panel);
  wireSelect('select-trg-source', 'source');
  wireSelect('select-trg-edge', 'edge');
  el<HTMLSelectElement>('select-trg-edge')?.addEventListener('change', () => {
    S.setSwpEdge(edgeOf(selectValue('select-trg-edge')));
  });
  wireSelect('select-trg-out', 'out');
  wireSelect('select-trg-outpolarity', 'outpolarity');
  wireNumber('input-trg-level', 'level');
  wireNumber('input-trg-safetime', 'safetime');
  wireNumber('input-trg-delay', 'delay');
  wireNumber('input-trg-pretime', 'pretime');
  wireNumber('input-trg-acqtime', 'acqtime');
  wireNumber('input-trg-retrigger', 'retrigger');
  wireNumber('input-trg-retriggerperiod', 'retriggerperiod');

  button()?.addEventListener('click', () => {
    if (phase === 'waiting') { disarm(); return; }
    if (S.rtaMode) arm();                // hardware device trigger
    else armSwept();                     // software trigger on consecutive sweeps
  });
  el('btn-trg-free')?.addEventListener('click', disarm);
  el('btn-trg-from-mkr')?.addEventListener('click', () => {
    const m = S.markers[0];
    const tr = S.traces[S.activeTraceIdx];
    if (!m?.enabled || !tr?.powers || !Number.isFinite(m.idx)) return;
    const v = tr.powers[m.idx];
    if (!Number.isFinite(v)) return;
    const i = el<HTMLInputElement>('input-trg-level');
    if (i) i.value = v.toFixed(1);
    push({ level: Number(v.toFixed(1)) });
  });
  el('btn-trg-from-peak')?.addEventListener('click', () => {
    const peak = peakOfDisplay();
    if (peak === null) return;
    const i = el<HTMLInputElement>('input-trg-level');
    if (i) i.value = (peak - 10).toFixed(1);
    push({ level: Number((peak - 10).toFixed(1)) });
  });
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && phase !== 'free') disarm();
  });
  onSwpHit((freqHz, level) => {
    phase = 'hit';
    hitAt = fmtClock(new Date());
    hitLine = t('trg_hit_level', { f: freqHz >= 1e9 ? `${(freqHz / 1e9).toFixed(4)} GHz` : `${(freqHz / 1e6).toFixed(3)} MHz`, v: level.toFixed(1) });
    syncOverlay();
    renderAll();
  });
  onTriggerHit(() => {
    if (!S.rtaMode) return;                    // SWP has its own software detection
    // packet arrived while armed -> the capture is on screen right now
    phase = 'hit';
    hitAt = fmtClock(new Date());
    S.setTrigArmed(false);
    syncOverlay();
    renderAll();
  });
  window.setInterval(() => {
    if (phase !== 'waiting') return;
    syncOverlay();
    renderAll();            // repaint so the elapsed seconds actually tick
  }, TICK_MS);
  onLangChange(() => { syncButton(); lastOverlayKey = ''; syncOverlay(); renderAll(); });
  syncButton();
  schedule();
}
