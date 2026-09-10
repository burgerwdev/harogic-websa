// Trigger panel: RTA acquisition trigger, on-demand capture and live state.
//
// Behaviour contract (measured on the SAN-90):
//  - the default source is `bus`: the host bus-triggers every frame, so the display runs
//    live at ~100 fps ("平时自由看");
//  - a level trigger only fires on a threshold CROSSING after arming, and while it waits
//    the device produces no packets, so the plot stays on the last frame. That is a
//    normal state, not a hang — hence the on-canvas WAITING overlay;
//  - "Capture" therefore arms a one-shot level trigger: the next crossing is captured and
//    the panel returns to free run automatically, so the live view resumes by itself.
import * as S from '../core/store';
import { applyI18n, t } from '../core/i18n';
import { send } from '../core/wsSend';
import { renderAll } from '../render/spectrum';
import { getDisplayPowers } from '../dsp/peaks';

const POLL_MS = 800;
const POLL_ARMED_MS = 250;
const ONE_SHOT_TIMEOUT_MS = 30_000;

let lastFrames = -1;
let lastBadge = '';
let lastHint = '';
let oneShot = false;
let armedConfirmed = false;
let armedAt = 0;
let hintUntil = 0;

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function fmtTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '-';
  if (seconds < 1e-3) return `${Math.round(seconds * 1e6)} \u00b5s`;
  if (seconds < 1) return `${(seconds * 1e3).toFixed(1)} ms`;
  return `${seconds.toFixed(2)} s`;
}

function push(payload: Record<string, unknown>): void {
  send({ cmd: 'SET_TRIGGER', ...payload });
}

function setText(id: string, text: string, cls = ''): void {
  const e = el(id);
  if (!e) return;
  if (e.textContent !== text) e.textContent = text;
  const want = cls ? `cur-val ${cls}` : 'cur-val';
  if (e.className !== want && e.classList.contains('cur-val')) e.className = want;
}

function setHint(key: string, params?: Record<string, string | number>, holdMs = 0): void {
  const text = key ? t(key, params) : '-';
  if (text !== lastHint) {
    lastHint = text;
    setText('trigger-hint', text);
  }
  if (holdMs) hintUntil = performance.now() + holdMs;
}

/** Level of the strongest displayed bin, used for the "will it ever trigger" hint.
 *  Uses the same accessor as the renderer/marker code: the RTA accumulation buffers can
 *  be empty or zero-filled, so reading them directly reports a bogus 0 dBm peak. */
function peakLevel(): number | null {
  const d = getDisplayPowers();
  if (!d) return null;
  let peak = -Infinity;
  for (let i = 0; i < d.length; i++) {
    const v = d[i];
    if (Number.isFinite(v) && v > peak) peak = v;
  }
  return Number.isFinite(peak) ? peak : null;
}

function syncFields(req: Record<string, any>): void {
  const setSel = (id: string, value: unknown) => {
    const s = el<HTMLSelectElement>(id);
    if (s && document.activeElement !== s && value !== undefined && value !== null) s.value = String(value);
  };
  const setNum = (id: string, value: unknown) => {
    const i = el<HTMLInputElement>(id);
    if (i && document.activeElement !== i && Number.isFinite(Number(value))) i.value = String(Number(value));
  };
  setSel('select-trg-source', req.trigger_source);
  setSel('select-trg-edge', req.trigger_edge);
  setNum('input-trg-level', req.trigger_level);
  setNum('input-trg-safetime', req.trigger_safetime);
  setNum('input-trg-delay', req.trigger_delay);
  setNum('input-trg-pretime', req.trigger_pretime);
  setNum('input-trg-acqtime', req.trigger_acqtime);
  setNum('input-trg-retrigger', req.trigger_retrigger);
  setNum('input-trg-retriggerperiod', req.trigger_retriggerperiod);
  setSel('select-trg-out', req.trigger_out);
  setSel('select-trg-outpolarity', req.trigger_outpolarity);
}

function disarm(reasonKey: string, holdMs = 4000): void {
  oneShot = false;
  const s = el<HTMLSelectElement>('select-trg-source');
  if (s) s.value = 'bus';
  push({ source: 'bus' });
  if (reasonKey) setHint(reasonKey, undefined, holdMs);
  S.setTrigArmed(false);
  S.setTrigWaiting(false);
  lastBadge = '';                       // reflect the change immediately, don't wait for a poll
  updateBadge(false, false, false);
  renderAll();
}

function updateBadge(advanced: boolean, armed: boolean, waiting: boolean): void {
  let key = 'trg_st_free';
  let cls = '';
  if (armed) {
    if (waiting) { key = 'trg_st_wait'; cls = 'fail'; }
    else { key = 'trg_st_trig'; cls = 'pass'; }
  }
  const text = t(key);
  if (text + cls !== lastBadge) {
    lastBadge = text + cls;
    setText('trigger-status', text, cls);
  }
  void advanced;
}

function updateHint(armed: boolean, waiting: boolean, level: number): void {
  if (performance.now() < hintUntil) return;      // transient message still on screen
  if (!armed) { setHint(''); return; }
  const peak = peakLevel();
  if (peak !== null && level > peak) { setHint('trg_hint_above', { db: (level - peak).toFixed(1) }); return; }
  if (waiting) { setHint('trg_hint_cross'); return; }
  setHint('trg_hint_capturing');
}

async function pollOnce(): Promise<void> {
  const body = document.querySelector('#trigger-panel .group-body') as HTMLElement | null;
  if (body && body.offsetParent === null) return;      // panel collapsed: nothing to update
  try {
    const r = await fetch('/api/state', { cache: 'no-store' });
    const d = await r.json();
    const req = (d?.req?.rta ?? {}) as Record<string, any>;
    const status = (req.trigger_actual ?? {}) as Record<string, any>;
    const source = String(req.trigger_source ?? 'bus');
    const armed = source !== 'bus' && source !== 'freerun';
    const frames = Number(status.frames ?? -1);
    const advanced = lastFrames >= 0 && frames > lastFrames;
    const waiting = armed ? (status.waiting === true || !advanced) : false;

    S.setTrigSource(source);
    S.setTrigLevel(Number(req.trigger_level ?? -40));
    S.setTrigPoi(Number((d?.rta_actual ?? {}).poi ?? 0));
    S.setTrigArmed(armed);
    S.setTrigWaiting(waiting);
    syncFields(req);

    // one-shot: wait until the device confirms the armed source before trusting the frame
    // counter (frames still arrive for a moment while the reconfiguration is in flight)
    if (oneShot && armed) {
      if (!armedConfirmed) {
        armedConfirmed = true;
        lastFrames = frames;               // fresh baseline
      } else if (advanced) {
        disarm('trg_hint_captured', 4000);
      } else if (performance.now() - armedAt > ONE_SHOT_TIMEOUT_MS) {
        disarm('trg_hint_timeout', 6000);
      }
    }
    lastFrames = frames;

    const lvl = Number((el<HTMLInputElement>('input-trg-level')?.value ?? req.trigger_level ?? -40));
    updateBadge(advanced, armed, waiting);
    updateHint(S.trigArmed, S.trigWaiting, lvl);
    const poi = el('trigger-poi');
    if (poi) {
      const text = S.trigPoi > 0 ? t('trg_poi', { t: fmtTime(S.trigPoi) }) : '-';
      if (poi.textContent !== text) poi.textContent = text;
    }
    renderAll();          // redraws the threshold line and the waiting overlay
  } catch {
    /* device offline: keep the last known state */
  }
}

function schedule(): void {
  const armed = S.trigArmed;
  window.setTimeout(() => { void pollOnce().finally(schedule); }, armed && oneShot ? POLL_ARMED_MS : POLL_MS);
}

function armOneShot(): void {
  const input = el<HTMLInputElement>('input-trg-level');
  const level = parseFloat(input?.value ?? '');
  if (!Number.isFinite(level)) return;
  const s = el<HTMLSelectElement>('select-trg-source');
  if (s) s.value = 'level';
  oneShot = true;
  armedConfirmed = false;
  armedAt = performance.now();
  push({ source: 'level', level });
  S.setTrigArmed(true);
  S.setTrigWaiting(true);
  lastBadge = '';
  updateBadge(false, true, true);       // show "Waiting" at once; the poll confirms it
  setHint('trg_hint_arming', undefined, 3000);
}

function levelFromMarker(): number | null {
  const m = S.markers[0];
  const tr = S.traces[S.activeTraceIdx];
  if (!m?.enabled || !tr?.powers || !Number.isFinite(m.idx)) return null;
  const v = tr.powers[m.idx];
  return Number.isFinite(v) ? v : null;
}

function setLevel(value: number): void {
  const i = el<HTMLInputElement>('input-trg-level');
  if (i) i.value = value.toFixed(1);
  push({ level: Number(value.toFixed(1)) });
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
    if (key === 'source') {
      oneShot = false;
      armedConfirmed = false;
      if (s.value === 'level') armedAt = performance.now();
      setHint('');
    }
    push({ [key]: s.value });
  });
}

export function initTrigger(): void {
  const panel = document.getElementById('trigger-panel');
  if (panel) applyI18n(panel);
  wireSelect('select-trg-source', 'source');
  wireSelect('select-trg-edge', 'edge');
  wireSelect('select-trg-out', 'out');
  wireSelect('select-trg-outpolarity', 'outpolarity');
  wireNumber('input-trg-level', 'level');
  wireNumber('input-trg-safetime', 'safetime');
  wireNumber('input-trg-delay', 'delay');
  wireNumber('input-trg-pretime', 'pretime');
  wireNumber('input-trg-acqtime', 'acqtime');
  wireNumber('input-trg-retrigger', 'retrigger');
  wireNumber('input-trg-retriggerperiod', 'retriggerperiod');

  el('btn-trg-capture')?.addEventListener('click', armOneShot);
  el('btn-trg-free')?.addEventListener('click', () => { armedConfirmed = false; disarm('', 0); });
  el('btn-trg-from-mkr')?.addEventListener('click', () => {
    const v = levelFromMarker();
    if (v !== null) setLevel(v);
  });
  el('btn-trg-from-peak')?.addEventListener('click', () => {
    const peak = peakLevel();
    if (peak !== null) setLevel(peak - 10);
  });

  schedule();
  window.addEventListener('focus', () => { void pollOnce(); });
}
