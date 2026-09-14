// Frequency panel: SWP/RTA editors, span stepping and the apply actions.
// Extracted from ui/controls.ts (report finding P1-5); controls.ts re-exports it so the
// public surface is unchanged.
import * as S from '../../core/store';
import { niceSpanStep, normalizeCenterSpan, normalizeStartStop, steppedSpan } from '../../core/frequency';
import { parseFreqUnit, toUnit } from '../../core/units';
import { send } from '../../core/wsSend';
import { centerHz, spanHz } from '../freqState';
import { spanStepAuto, spanStepHz } from '../swpState';
import { units } from '../../core/units';

export function frequencyEditor(id: 'swp-freq-settings' | 'rta-freq-settings'): HTMLElement | null {
  return document.getElementById(id);
}

export function markFrequencyDirty(id: 'swp-freq-settings' | 'rta-freq-settings') {
  const editor = frequencyEditor(id);
  if (editor) editor.dataset.dirty = '1';
}

export function beginFrequencyCommit(id: 'swp-freq-settings' | 'rta-freq-settings') {
  const editor = frequencyEditor(id);
  if (editor) {
    editor.dataset.pending = '1';
    editor.dataset.pendingVersion = String(S.configVersion + 1);
    editor.dataset.pendingAt = String(Date.now());
  }
}

function clearFrequencyEditor(editor: HTMLElement) {
  delete editor.dataset.pending;
  delete editor.dataset.pendingVersion;
  delete editor.dataset.pendingAt;
  delete editor.dataset.dirty;
  editor.querySelectorAll('input').forEach(input => delete (input as HTMLElement).dataset.edited);
}

export function syncFrequencyEditorStatus(responseTo?: string, configVersion = 0) {
  const scalarInput = responseTo === 'SET_RBW'
    ? 'input-rbw'
    : responseTo === 'SET_VBW' ? 'input-vbw' : responseTo === 'SET_PNM' ? 'input-pnm' : null;
  if (scalarInput) {
    const input = document.getElementById(scalarInput);
    if (input) delete input.dataset.edited;
  }
  const id = responseTo === 'SET_FREQ'
    ? 'swp-freq-settings'
    : responseTo === 'SET_RTA' ? 'rta-freq-settings' : null;
  if (id) {
    const editor = frequencyEditor(id);
    if (editor) clearFrequencyEditor(editor);
    return;
  }
  for (const editorId of ['swp-freq-settings', 'rta-freq-settings'] as const) {
    const editor = frequencyEditor(editorId);
    if (!editor?.dataset.pending) continue;
    const expected = Number(editor.dataset.pendingVersion || Infinity);
    const pendingAt = Number(editor.dataset.pendingAt || Date.now());
    if (configVersion >= expected && Date.now() - pendingAt >= 2500) {
      clearFrequencyEditor(editor);
    }
  }
}

export function validateFrequencyWindow(window: unknown, ids: string[]): boolean {
  for (const id of ids) {
    const input = document.getElementById(id) as HTMLInputElement | null;
    if (input) input.setCustomValidity(window ? '' : 'Invalid frequency range');
  }
  if (!window) {
    (document.getElementById(ids[0]) as HTMLInputElement | null)?.reportValidity();
    return false;
  }
  return true;
}

export function applyCenterSpan() {
  const window = normalizeCenterSpan(
    parseFreqUnit('center'), parseFreqUnit('span'), S.FREQ_MIN, S.FREQ_MAX);
  if (!validateFrequencyWindow(window, ['input-center', 'input-span'])) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', center: window!.center, span: window!.span });
}

export function applyStartStop() {
  const window = normalizeStartStop(
    parseFreqUnit('start'), parseFreqUnit('stop'), S.FREQ_MIN, S.FREQ_MAX);
  if (!validateFrequencyWindow(window, ['input-start', 'input-stop'])) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', start: window!.start, stop: window!.stop });
}

export function applyFullSpan() {
  beginFrequencyCommit('swp-freq-settings');
  send({
    cmd: 'SET_FREQ',
    center: (S.FREQ_MIN + S.FREQ_MAX) / 2,
    span: S.FREQ_MAX - S.FREQ_MIN,
  });
}

function formatSpanStep(value: number): string {
  if (value >= 100) return value.toFixed(0);
  if (value >= 10) return value.toFixed(1).replace(/\.0$/, '');
  return value.toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
}

export function syncSwpSpanStep(swpSpan = spanHz.get()) {
  if (spanStepAuto.get()) spanStepHz.set(niceSpanStep(swpSpan));
  const input = document.getElementById('input-span-step') as HTMLInputElement | null;
  const unit = document.getElementById('span-step-unit');
  if (input && document.activeElement !== input) {
    input.value = formatSpanStep(toUnit(spanStepHz.get(), 'span'));
  }
  if (unit) unit.textContent = units().span;
  const auto = document.getElementById('btn-span-step-auto');
  if (auto) auto.classList.toggle('active', spanStepAuto.get());
}

export function updateCustomSpanStep() {
  const input = document.getElementById('input-span-step') as HTMLInputElement | null;
  if (!input) return;
  const value = Number(input.value);
  if (!isFinite(value) || value <= 0) return;
  const u = units().span;
  const scale = u === 'GHz' ? 1e9 : u === 'MHz' ? 1e6 : u === 'kHz' ? 1e3 : 1;
  spanStepAuto.set(false);
  spanStepHz.set(Math.max(100, value * scale));
  syncSwpSpanStep();
}

export function resetSpanStepAuto() {
  spanStepAuto.set(true);
  syncSwpSpanStep();
}

export function stepSwpSpan(direction: -1 | 1) {
  const targetSpan = steppedSpan(
    spanHz.get(),
    spanStepHz.get(),
    direction,
    100,
    S.FREQ_MAX - S.FREQ_MIN,
  );
  if (targetSpan === spanHz.get()) return;
  const window = normalizeCenterSpan(
    centerHz.get(), targetSpan, S.FREQ_MIN, S.FREQ_MAX);
  if (!window) return;
  beginFrequencyCommit('swp-freq-settings');
  send({ cmd: 'SET_FREQ', center: window.center, span: window.span });
}
