// Resolution panel: RBW / VBW / points / window / spur actions.
import { parseFreqUnit, toUnit, units } from '../../core/units';
import { send } from '../../core/wsSend';
import { clearRtaAccum } from './rta';
import { currentRBW, currentVBW, rbwMode, vbwMode } from '../swpState';
import { currentGraphMode, graphModeDiverges, measurementOwnsDevice } from '../graphMode';

type ResolutionField = 'rbw' | 'vbw';
const fields = {
  rbw: { value: currentRBW, mode: rbwMode, command: 'SET_RBW', modes: ['auto', 'manual', 'one_thousandth_span', 'one_percent_span'] },
  vbw: { value: currentVBW, mode: vbwMode, command: 'SET_VBW', modes: ['bypass', 'equal', 'tenth', 'onethousandth', 'manual'] },
};
type ResolutionCommit = {
  requestId: string; activeMode: string; generation: string; text: string; unit: string | undefined; timer: number;
  earlierRequests: Set<string>; rollback: { value: number; mode: string; serial: number };
};
const commits = new Map<ResolutionField, ResolutionCommit>();
const latestRequests = new Map<ResolutionField, string>();
let requestSerial = 0;

function inputFor(field: ResolutionField): HTMLInputElement | null {
  return document.getElementById(`input-${field}`) as HTMLInputElement | null;
}

function selectFor(field: ResolutionField): HTMLSelectElement | null {
  return document.getElementById(`select-${field}-mode`) as HTMLSelectElement | null;
}

/** Every edit has a generation, including edits made while a request is in flight. */
export function markResolutionDirty(field: ResolutionField): void {
  const input = inputFor(field);
  if (!input) return;
  input.dataset.edited = '1';
  input.dataset.dirtyGen = String(Number(input.dataset.dirtyGen || '0') + 1);
  input.setCustomValidity('');
}

function renderResolution(field: ResolutionField): void {
  const input = inputFor(field);
  if (input && input.dataset.edited !== '1') input.value = toUnit(fields[field].value.get(), field).toFixed(2);
  const select = selectFor(field);
  if (select) select.value = fields[field].mode.get();
}

function settleResolution(field: ResolutionField): void {
  const commit = commits.get(field);
  if (!commit) return;
  clearTimeout(commit.timer);
  commits.delete(field);
  fields[field].value.reset();
  fields[field].mode.reset();
  fields[field].value.confirm(commit.rollback.value);
  fields[field].mode.confirm(commit.rollback.mode);
  const input = inputFor(field);
  if (input) {
    delete input.dataset.pending;
    // Keep both generation and text guards: keypad/unit actions can edit programmatically.
    const changed = (input.dataset.dirtyGen || '0') !== commit.generation
      || input.value !== commit.text || units()[field] !== commit.unit;
    if (!changed) delete input.dataset.edited;
  }
  const select = selectFor(field);
  if (select) delete select.dataset.pending;
}

function applyResolution(field: ResolutionField): void {
  if (currentGraphMode() === 'sdr' || measurementOwnsDevice() || graphModeDiverges()) return;
  const setting = fields[field];
  const select = selectFor(field);
  const mode = select?.value || setting.mode.get();
  const supported = setting.modes.includes(mode)
    && !(field === 'rbw' && currentGraphMode() === 'rta' && mode !== 'auto' && mode !== 'manual');
  if (select) select.setCustomValidity(supported ? '' : 'This bandwidth mode is unavailable in the active mode');
  if (!supported) { select?.reportValidity(); return; }
  const input = inputFor(field);
  const value = mode === 'manual' ? parseFreqUnit(field) : null;
  if (value !== null && (!input?.value.trim() || !Number.isFinite(value) || value <= 0)) {
    input?.setCustomValidity('Enter a positive bandwidth');
    input?.reportValidity();
    return;
  }
  input?.setCustomValidity('');
  const previous = commits.get(field);
  if (previous) clearTimeout(previous.timer);
  const requestId = `acq-resolution-${++requestSerial}`;
  latestRequests.set(field, requestId);
  // Reset both slots together: automatic modes must not retain a prior numeric desire.
  setting.mode.reset();
  setting.value.reset();
  const rollback = previous?.rollback ?? { value: setting.value.get(), mode: setting.mode.get(), serial: 0 };
  setting.mode.set(mode);
  if (value !== null) setting.value.set(value);
  if (input) { input.dataset.pending = '1'; input.dataset.edited = '1'; }
  if (select) select.dataset.pending = '1';
  commits.set(field, {
    requestId, activeMode: currentGraphMode(), generation: input?.dataset.dirtyGen || '0',
    text: input?.value || '', unit: units()[field],
    earlierRequests: new Set(previous ? [...previous.earlierRequests, previous.requestId] : []),
    rollback,
    timer: window.setTimeout(() => {
      if (commits.get(field)?.requestId !== requestId) return;
      settleResolution(field);
      renderResolution(field);
    }, 3000),
  });
  if (currentGraphMode() === 'rta') clearRtaAccum();
  send({ cmd: setting.command, mode, ...(value === null ? {} : { [field]: value }), request_id: requestId });
}

export function applyRBW(): void { applyResolution('rbw'); }
export function applyVBW(): void { applyResolution('vbw'); }

/** Only the current request's reply may settle or publish its effective readback. */
export function confirmResolutionStatus(s: {
  mode: string; response_to?: string; request_id?: string | number;
  rbw?: number; vbw?: number; rbw_mode?: string; vbw_mode?: string;
}): void {
  const ownReply = typeof s.request_id === 'string' && s.request_id.startsWith('acq-resolution-');
  const target = s.response_to === 'SET_RBW' ? 'rbw' : s.response_to === 'SET_VBW' ? 'vbw' : null;
  // Older accepted commands update the rollback baseline only while a newer request waits.
  // They cannot settle its slots, and cannot publish after it has already settled.
  if (ownReply && (!target || latestRequests.get(target) !== s.request_id)) {
    const pending = target ? commits.get(target) : undefined;
    if (target && pending?.activeMode === s.mode && pending.earlierRequests.has(s.request_id as string)) {
      const serial = Number(String(s.request_id).slice('acq-resolution-'.length));
      const value = s[target];
      const mode = s[`${target}_mode`];
      if (serial > pending.rollback.serial && typeof value === 'number' && Number.isFinite(value) && value > 0 && typeof mode === 'string') {
        pending.rollback = { value, mode, serial };
      }
    }
    return;
  }
  for (const field of ['rbw', 'vbw'] as const) {
    if (ownReply && target !== field) continue;
    const pending = commits.get(field);
    if (pending && pending.activeMode !== s.mode) settleResolution(field);
    else if (pending) {
      if (target !== field || s.request_id !== pending.requestId) continue;
      settleResolution(field);
    }
    const value = s[field];
    const mode = s[`${field}_mode`];
    if (typeof value === 'number' && Number.isFinite(value) && value > 0) fields[field].value.confirm(value);
    if (typeof mode === 'string') fields[field].mode.confirm(mode);
    renderResolution(field);
  }
  syncResolutionAvailability(s.mode);
}

export function rejectResolutionCommit(responseTo?: string, requestId?: string | number): void {
  const field = responseTo === 'SET_RBW' ? 'rbw' : responseTo === 'SET_VBW' ? 'vbw' : null;
  if (!field || requestId === undefined || commits.get(field)?.requestId !== requestId) return;
  settleResolution(field);
  renderResolution(field);
}

/** Preset, socket loss and a confirmed mode change invalidate the old transaction. */
export function resetResolutionEditors(): void {
  for (const field of ['rbw', 'vbw'] as const) {
    settleResolution(field);
    fields[field].value.reset();
    fields[field].mode.reset();
    const input = inputFor(field);
    if (input) { delete input.dataset.edited; delete input.dataset.dirtyGen; input.setCustomValidity(''); }
    selectFor(field)?.setCustomValidity('');
    renderResolution(field);
  }
  latestRequests.clear();
}

export function syncResolutionAvailability(mode: string = currentGraphMode()): void {
  const disabled = (mode !== 'std' && mode !== 'rta') || measurementOwnsDevice() || graphModeDiverges();
  for (const field of ['rbw', 'vbw'] as const) {
    const select = selectFor(field);
    if (select) {
      select.disabled = disabled;
      select.setCustomValidity('');
      if (field === 'rbw') for (const option of Array.from(select.options)) {
        option.disabled = mode === 'rta' && option.value !== 'auto' && option.value !== 'manual';
      }
    }
    const input = inputFor(field);
    if (input) input.disabled = disabled;
    document.querySelectorAll<HTMLButtonElement>(`button[data-action="apply-${field}"]`)
      .forEach(button => { button.disabled = disabled; });
  }
}

export function applyPoints() {
  const el = document.getElementById('input-points') as HTMLInputElement;
  send({ cmd: 'SET_POINTS', points: parseInt(el?.value || '1001') || 1001 });
}

export function setSpurMode(mode: string) { send({ cmd: 'SET_SPUR', mode }); }

export function setFrameDetector(v: string) { send({ cmd: 'SET_FRAMEDET', mode: v }); }

export function setTraceAlign(v: string) { send({ cmd: 'SET_TRACEGRID', trace_align: v }); }

export function setPointsStrategy(v: string) { send({ cmd: 'SET_TRACEGRID', points_strategy: v }); }

export function setWindow(v: string) { send({ cmd: 'SET_WINDOW', window: parseInt(v) }); }
