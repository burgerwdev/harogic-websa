// Trigger panel: RTA acquisition trigger configuration, live status and POI hint.
//
// The device reports the trigger config and per-frame status through the STATUS payload
// (/api/state), so this module polls that endpoint while the panel is visible instead of
// touching the WebSocket frame path. Commands go out through the shared send() helper.
//
// Hardware facts this UI is built around (measured on the SAN-90):
//  - a level trigger is EDGE based: a carrier that stays above the threshold does not
//    produce further captures, so "Waiting" is a normal, stable state;
//  - POI (RTA_FrameInfo.POI) is the shortest burst the current configuration is
//    guaranteed to intercept, which is the only honest answer to "can I catch it?".
import * as S from '../core/store';
import { applyI18n, t } from '../core/i18n';
import { send } from '../core/wsSend';
import { renderAll } from '../render/spectrum';

const POLL_MS = 800;
let lastFrames = -1;
let lastBadge = '';

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
  window.setTimeout(refreshTrigger, 350);
}

/** Write device values back into the controls, leaving the focused control alone. */
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

function updateBadge(status: Record<string, any>, req: Record<string, any>): void {
  const badge = el('trigger-status');
  const poi = el('trigger-poi');
  const source = String(req.trigger_source ?? 'bus');
  const armed = source !== 'bus' && source !== 'freerun';
  if (badge) {
    let text = t('trg_st_free');
    let cls = '';
    if (armed) {
      const frames = Number(status.frames ?? -1);
      const advanced = lastFrames >= 0 && frames > lastFrames;
      text = advanced ? t('trg_st_trig') : t('trg_st_wait');
      cls = advanced ? 'pass' : 'fail';
      lastFrames = frames;
    } else {
      lastFrames = -1;
    }
    if (text + cls !== lastBadge) {
      lastBadge = text + cls;
      badge.textContent = text;
      badge.className = `cur-val trigger-status ${cls}`.trim();
    }
  }
  if (poi) {
    const seconds = Number(S.trigPoi);
    const text = seconds > 0 ? t('trg_poi', { t: fmtTime(seconds) }) : '-';
    if (poi.textContent !== text) poi.textContent = text;
  }
}

export function refreshTrigger(): void {
  fetch('/api/state', { cache: 'no-store' })
    .then((r) => r.json())
    .then((d) => {
      const req = (d?.req?.rta ?? {}) as Record<string, any>;
      const status = (req.trigger_actual ?? {}) as Record<string, any>;
      S.setTrigSource(String(req.trigger_source ?? 'bus'));
      S.setTrigLevel(Number(req.trigger_level ?? -40));
      S.setTrigPoi(Number((d?.rta_actual ?? {}).poi ?? 0));
      syncFields(req);
      updateBadge(status, req);
      renderAll();          // the threshold line follows the level
    })
    .catch(() => { /* device offline: keep the last known values */ });
}

/** Marker 1 amplitude, used by the "from marker" convenience button. */
function markerLevel(): number | null {
  const m = S.markers[0];
  const t0 = S.traces[S.activeTraceIdx];
  if (!m || !m.enabled || !t0?.powers || !Number.isFinite(m.idx)) return null;
  const v = t0.powers[m.idx];
  return Number.isFinite(v) ? v : null;
}

/** Strongest displayed level, used by the "peak -10 dB" convenience button. */
function peakLevel(): number | null {
  const t0 = S.traces[S.activeTraceIdx];
  const d = t0?.powers;
  if (!d) return null;
  let peak = -Infinity;
  for (let i = 0; i < d.length; i++) if (d[i] > peak) peak = d[i];
  return Number.isFinite(peak) ? peak : null;
}

function setLevel(value: number): void {
  const i = el<HTMLInputElement>('input-trg-level');
  if (i) i.value = value.toFixed(1);
  push({ level: Number(value.toFixed(1)) });
}

function wireNumber(id: string, key: string): void {
  const i = el<HTMLInputElement>(id);
  if (!i) return;
  i.addEventListener('change', () => {
    const v = parseFloat(i.value);
    if (Number.isFinite(v)) push({ [key]: v });
  });
}

function wireSelect(id: string, key: string): void {
  const s = el<HTMLSelectElement>(id);
  if (!s) return;
  s.addEventListener('change', () => push({ [key]: s.value }));
}

export function initTrigger(): void {
  applyI18n(document.getElementById('trigger-panel') ?? document);
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

  el('btn-trg-free')?.addEventListener('click', () => {
    const s = el<HTMLSelectElement>('select-trg-source');
    if (s) s.value = 'bus';
    push({ source: 'bus' });
  });
  el('btn-trg-from-mkr')?.addEventListener('click', () => {
    const v = markerLevel();
    if (v !== null) setLevel(v);
  });
  el('btn-trg-from-peak')?.addEventListener('click', () => {
    const v = peakLevel();
    if (v !== null) setLevel(v - 10);
  });

  refreshTrigger();
  // Only poll while the panel is actually visible: the status is a convenience, not a
  // data path, and the device only needs to be read when the user can see the result.
  window.setInterval(() => {
    const body = document.querySelector('#trigger-panel .group-body') as HTMLElement | null;
    if (body && body.offsetParent !== null) refreshTrigger();
  }, POLL_MS);
  window.addEventListener('focus', refreshTrigger);
}
