// Frequency panel: SWP/RTA editors, span stepping and the apply actions.
import * as S from '../../core/store';
import { niceSpanStep, normalizeCenterSpan, normalizeStartStop, steppedSpan } from '../../core/frequency';
import { parseFreqUnit, toUnit } from '../../core/units';
import { send } from '../../core/wsSend';
import { centerHz, spanHz, rtaCenterHz } from '../freqState';
import { updateFreqUIInputs } from '../freqInputs';
import { spanStepAuto, spanStepHz } from '../swpState';
import { units } from '../../core/units';
import { currentGraphMode } from '../graphMode';

export function frequencyEditor(id: 'swp-freq-settings' | 'rta-freq-settings'): HTMLElement | null {
  return document.getElementById(id);
}

export function markFrequencyDirty(id: 'swp-freq-settings' | 'rta-freq-settings') {
  const editor = frequencyEditor(id);
  if (!editor) return;
  editor.dataset.dirty = '1';
  // Every keystroke bumps the generation: an ack may then tell edits made BEFORE a commit
  // apart from edits typed while that commit was in flight (see syncFrequencyEditorStatus).
  editor.dataset.dirtyGen = String(Number(editor.dataset.dirtyGen || '0') + 1);
}

type EditorId = 'swp-freq-settings' | 'rta-freq-settings';
type FrequencyWindow = { center: number; span: number };
type FrequencyCommit = { requestId: string; window?: FrequencyWindow; accepted?: FrequencyWindow; timer: number };
const commits = new Map<EditorId, FrequencyCommit>();
const latestRequests = new Map<EditorId, string>();
let requestSerial = 0;
let statusMode: string | null = null;

export function beginFrequencyCommit(id: EditorId, window?: FrequencyWindow): string {
  const requestId = `acq-frequency-${++requestSerial}`;
  const previous = commits.get(id);
  if (previous) clearTimeout(previous.timer);
  latestRequests.set(id, requestId);
  const editor = frequencyEditor(id);
  if (editor) {
    editor.dataset.pending = '1';
    editor.dataset.pendingAt = String(Date.now());
    editor.dataset.requestId = requestId;
    editor.dataset.commitGen = editor.dataset.dirtyGen || '0';
  }
  commits.set(id, { requestId, window, timer: globalThis.window.setTimeout(() => {
    if (commits.get(id)?.requestId !== requestId) return;
    settleFrequencyCommit(id);
    updateFreqUIInputs(true);
  }, 3000) });
  projectFrequencyIntent(id);
  return requestId;
}

function projectFrequencyIntent(id: EditorId) {
  const window = commits.get(id)?.window;
  if (!window || (id === 'rta-freq-settings' ? 'rta' : 'std') !== currentGraphMode()) return;
  centerHz.set(window.center);
  spanHz.set(window.span);
  if (id === 'rta-freq-settings') rtaCenterHz.set(window.center);
}

function clearFrequencyEditor(editor: HTMLElement) {
  delete editor.dataset.pending;
  delete editor.dataset.pendingAt;
  delete editor.dataset.requestId;
  delete editor.dataset.commitGen;
  delete editor.dataset.dirty;
  editor.querySelectorAll('input').forEach(input => delete (input as HTMLElement).dataset.edited);
}

function settleFrequencyCommit(id: EditorId) {
  const pending = commits.get(id);
  if (pending) clearTimeout(pending.timer);
  commits.delete(id);
  const editor = frequencyEditor(id);
  if (editor) {
    const hasNewDraft = (editor.dataset.dirtyGen || '0') !== (editor.dataset.commitGen || '0');
    if (hasNewDraft) {
      delete editor.dataset.pending;
      delete editor.dataset.pendingAt;
      delete editor.dataset.requestId;
      delete editor.dataset.commitGen;
    } else clearFrequencyEditor(editor);
  }
  if ((id === 'rta-freq-settings' ? 'rta' : 'std') === currentGraphMode()) {
    centerHz.reset();
    spanHz.reset();
  }
  if (id === 'rta-freq-settings') rtaCenterHz.reset();
}

export function syncFrequencyEditorStatus(responseTo?: string, requestId?: string | number) {
  const id = responseTo === 'SET_FREQ' ? 'swp-freq-settings'
    : responseTo === 'SET_RTA' ? 'rta-freq-settings' : null;
  if (id && requestId !== undefined && commits.get(id)?.requestId === requestId) settleFrequencyCommit(id);
}

export function confirmFrequencyStatus(s: {
  response_to?: string; request_id?: string | number; mode: string;
  center: number; span: number; req: { rta?: { center?: number } };
}) {
  const id = s.response_to === 'SET_FREQ' ? 'swp-freq-settings'
    : s.response_to === 'SET_RTA' ? 'rta-freq-settings' : null;
  // A superseded success can update rollback state while the newer intent stays visible.
  // Once that newer intent settles (or Preset retires it), an older reply cannot publish.
  if (id && typeof s.request_id === 'string' && s.request_id.startsWith('acq-frequency-')
      && latestRequests.get(id) !== s.request_id && !commits.has(id)) return;
  if (statusMode !== s.mode) {
    centerHz.reset();
    spanHz.reset();
    statusMode = s.mode;
  }
  syncFrequencyEditorStatus(s.response_to, s.request_id);
  if (Number(s.req.rta?.center) > 0) rtaCenterHz.confirm(Number(s.req.rta?.center));
  if (id && commits.has(id)) commits.get(id)!.accepted = { center: Number(s.center), span: Number(s.span) };
  centerHz.confirm(Number(s.center));
  spanHz.confirm(Number(s.span));
  projectFrequencyIntent(s.mode === 'rta' ? 'rta-freq-settings' : 'swp-freq-settings');
}
export function rejectFrequencyCommit(responseTo?: string, requestId?: string | number) {
  const id = responseTo === 'SET_FREQ' ? 'swp-freq-settings'
    : responseTo === 'SET_RTA' ? 'rta-freq-settings' : null;
  const commit = id ? commits.get(id) : undefined;
  if (id && commit && requestId === commit.requestId && commit.accepted) {
    settleFrequencyCommit(id);
    centerHz.confirm(commit.accepted.center);
    spanHz.confirm(commit.accepted.span);
  } else syncFrequencyEditorStatus(responseTo, requestId);
  updateFreqUIInputs(true);
}

export function resetFrequencyEditors() {
  for (const id of ['swp-freq-settings', 'rta-freq-settings'] as const) {
    settleFrequencyCommit(id);
    const editor = frequencyEditor(id);
    if (editor) clearFrequencyEditor(editor);
  }
  latestRequests.clear();
  statusMode = null;
  centerHz.reset();
  spanHz.reset();
  rtaCenterHz.reset();
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
  const request_id = beginFrequencyCommit('swp-freq-settings', window!);
  send({ cmd: 'SET_FREQ', center: window!.center, span: window!.span, request_id });
}

export function applyStartStop() {
  const window = normalizeStartStop(
    parseFreqUnit('start'), parseFreqUnit('stop'), S.FREQ_MIN, S.FREQ_MAX);
  if (!validateFrequencyWindow(window, ['input-start', 'input-stop'])) return;
  const request_id = beginFrequencyCommit('swp-freq-settings', window!);
  send({ cmd: 'SET_FREQ', start: window!.start, stop: window!.stop, request_id });
}

export function applyFullSpan() {
  const window = { center: (S.FREQ_MIN + S.FREQ_MAX) / 2, span: S.FREQ_MAX - S.FREQ_MIN };
  const request_id = beginFrequencyCommit('swp-freq-settings', window);
  send({ cmd: 'SET_FREQ', ...window, request_id });
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
  const request_id = beginFrequencyCommit('swp-freq-settings', window);
  send({ cmd: 'SET_FREQ', center: window.center, span: window.span, request_id });
}
