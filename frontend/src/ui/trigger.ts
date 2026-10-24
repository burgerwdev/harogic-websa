// Trigger actions: standard mode uses software crossings; RTA uses device capture metadata.
import * as S from '../core/store';
import { applyI18n, onLangChange, t } from '../core/i18n';
import { send } from '../core/wsSend';
import { requestRender } from '../render/redraw';
import {
  canCapture, canEditTrigger, renderTriggerParameters, resetTrigger, syncTriggerMode,
  triggerFields, triggerMode, trigLevel, trigPoi, validTriggerValue, type TriggerKey,
} from './triggerState';
import type { Param } from '../core/params';
import { beginHardwareCapture, onTriggerHit, onTriggerReset, releaseHardwareCapture, triggerMissed, triggerUnsupported } from './triggerEvents';
import { armSwpTrigger, onSwpHit } from './swpTrigger';

let phase: 'free' | 'waiting' | 'hit' = 'free';
let since = 0;
let hitAt = '';
let hitLine = '';
let armPeak: number | null = null;

/** Threshold helpers read native dBm, before display offsets or trace normalization. */
function currentPowers(): Float32Array | null {
  return triggerMode() === 'rta' ? S.rtaData?.spec ?? null : S.traces[S.activeTraceIdx]?.raw ?? null;
}
function peakOfDisplay(): number | null {
  const powers = currentPowers();
  if (!powers) return null;
  let peak = -Infinity;
  for (const value of powers) if (Number.isFinite(value) && value > peak) peak = value;
  return Number.isFinite(peak) ? peak : null;
}
function clock(): string { return new Date().toLocaleTimeString([], { hour12: false }); }
function syncOverlay(): void {
  const lines: string[] = [];
  if (phase === 'waiting') {
    const seconds = Math.max(0, Math.floor((performance.now() - since) / 1000));
    lines.push(`${t('trg_chip_wait')} ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`);
    if (armPeak !== null && trigLevel.get() > armPeak) lines.push(`!${t('trg_never', { db: (trigLevel.get() - armPeak).toFixed(1) })}`);
    else lines.push(t(triggerMode() === 'rta' ? 'trg_cross' : 'trg_swp_cross'));
    if (armPeak !== null) lines.push(t('trg_peak', { v: armPeak.toFixed(1) }));
    if (triggerMissed()) lines.push(`!${t('trg_missed')}`);
    if (triggerUnsupported()) lines.push(`!${t('trg_unsupported')}`);
    if (trigPoi.get() > 0) lines.push(t('trg_poi', { t: `${(trigPoi.get() * 1000).toFixed(2)} ms` }));
  } else if (phase === 'hit') {
    lines.push(`${t(triggerMissed() ? 'trg_chip_missed' : 'trg_chip_hit')} ${hitAt}`);
    if (hitLine) lines.push(hitLine);
    if (triggerMissed()) lines.push(`!${t('trg_missed')}`);
    lines.push(t('trg_hit_hint'));
  }
  S.setTrigOverlay(lines);
  const button = document.getElementById('btn-trg-capture');
  if (button) {
    button.textContent = t(phase === 'waiting' ? 'trg_btn_stop' : phase === 'hit' ? 'trg_btn_again' : 'trg_btn_capture');
    button.classList.toggle('active', phase === 'waiting');
    button.setAttribute('aria-pressed', String(phase === 'waiting'));
  }
}


function restoreField(key: TriggerKey): void {
  triggerFields[key].slot.reset();
  renderTriggerParameters(true);
}
function readField(key: TriggerKey): string | number | null {
  const input = document.getElementById(triggerFields[key].id) as HTMLInputElement | HTMLSelectElement | null;
  if (!input) return triggerFields[key].slot.get();
  const value = validTriggerValue(key, input.value);
  if (value === null || !input.checkValidity()) { restoreField(key); return null; }
  return value;
}
function commitField(key: TriggerKey, value: string | number): void {
  // Field validation above proves the value belongs to this heterogeneous slot.
  const slot = triggerFields[key].slot as Param<string | number>;
  slot.set(value);
  send({ cmd: 'SET_TRIGGER', [key]: value });
  syncOverlay();
  requestRender();
}
function capture(): void {
  if (!canCapture()) return;
  const level = readField('level');
  const edge = readField('edge');
  if (typeof level !== 'number' || typeof edge !== 'string') return;
  const peak = peakOfDisplay();
  if (triggerMode() === 'std') {
    commitField('level', level); commitField('edge', edge);
    armSwpTrigger();
  } else {
    // Validate all current edits before changing the display or arming the hardware.
    const payload: Record<string, string | number> = { source: 'level', level, edge };
    for (const key of Object.keys(triggerFields) as TriggerKey[]) {
      if (key === 'source') continue;
      const value = readField(key);
      if (value === null) return;
      payload[key] = value;
    }
    if (!beginHardwareCapture()) { restoreField('level'); return; }
    for (const [key, value] of Object.entries(payload)) {
      const slot = triggerFields[key as TriggerKey].slot as Param<string | number>;
      slot.set(value);
    }
    send({ cmd: 'SET_TRIGGER', ...payload });
  }
  armPeak = peak;
  phase = 'waiting'; since = performance.now(); hitAt = ''; hitLine = '';
  renderTriggerParameters(true); syncOverlay(); requestRender();
}
function freeRun(): void {
  if (!canCapture()) return;
  resetTrigger();
  if (triggerMode() === 'rta') { releaseHardwareCapture(); commitField('source', 'bus'); }
  renderTriggerParameters(true);
}
function setThreshold(value: number): void {
  if (!canCapture()) return;
  const rounded = Number(value.toFixed(1));
  if (validTriggerValue('level', String(rounded)) === null) { restoreField('level'); return; }
  commitField('level', rounded);
  renderTriggerParameters(true);
}

export function initTrigger(): void {
  const panel = document.getElementById('trigger-panel');
  if (panel) applyI18n(panel);
  for (const key of Object.keys(triggerFields) as TriggerKey[]) {
    const input = document.getElementById(triggerFields[key].id) as HTMLInputElement | HTMLSelectElement | null;
    input?.addEventListener('change', () => {
      if (!canEditTrigger(key)) { restoreField(key); return; }
      const value = readField(key);
      if (value === null) return;
      if (key === 'source') {
        if (value === 'bus' || value === 'freerun') freeRun();
        else if (value === 'level') capture();
        else restoreField(key);
        return;
      }
      commitField(key, value);
    });
    input?.addEventListener('blur', () => renderTriggerParameters(true));
  }
  document.getElementById('btn-trg-capture')?.addEventListener('click', () => phase === 'waiting' ? freeRun() : capture());
  document.getElementById('btn-trg-free')?.addEventListener('click', freeRun);
  document.getElementById('btn-trg-from-mkr')?.addEventListener('click', () => {
    const marker = S.markers.find((m) => m.id === S.activeMkrId);
    const powers = currentPowers();
    if (marker?.enabled && powers && Number.isInteger(marker.idx) && Number.isFinite(powers[marker.idx])) setThreshold(powers[marker.idx]);
  });
  document.getElementById('btn-trg-from-peak')?.addEventListener('click', () => {
    const peak = peakOfDisplay();
    if (peak !== null) setThreshold(peak - 10);
  });
  window.addEventListener('keydown', (event) => { if (event.key === 'Escape' && phase !== 'free') freeRun(); });
  onTriggerReset(() => { phase = 'free'; since = 0; hitAt = ''; hitLine = ''; armPeak = null; syncOverlay(); requestRender(); });
  onSwpHit((freq, level) => {
    phase = 'hit'; hitAt = clock();
    hitLine = t('trg_hit_level', { f: freq >= 1e9 ? `${(freq / 1e9).toFixed(4)} GHz` : `${(freq / 1e6).toFixed(3)} MHz`, v: level.toFixed(1) });
    syncOverlay(); requestRender();
  });
  onTriggerHit(() => { phase = 'hit'; hitAt = clock(); syncOverlay(); requestRender(); });
  window.setInterval(() => {
    renderTriggerParameters();
    if (S.trigWaiting && phase === 'free' && triggerMode() === 'rta') { phase = 'waiting'; since = performance.now(); }
    if (phase !== 'free') { syncOverlay(); requestRender(); }
  }, 1000);
  onLangChange(() => { syncOverlay(); requestRender(); });
  syncTriggerMode(triggerMode());
  renderTriggerParameters(); syncOverlay();
}
