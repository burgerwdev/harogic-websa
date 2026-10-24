import * as S from '../core/store';
import { createParam, resetAll, type Param } from '../core/params';
import { resetTriggerRuntime } from './triggerEvents';

export type TriggerEdge = 'rising' | 'falling' | 'double';
const numberOptions = { parse: Number, serialize: String, equals: (a: number, b: number) => Math.abs(a - b) < 1e-9 };
const numeric = (key: string, fallback: number) => createParam<number>(`trigger.${key}`, { fallback, scope: 'trigger', ...numberOptions });
export const trigSource = createParam<string>('trigger.source', { fallback: 'bus', scope: 'trigger' });
export const trigLevel = numeric('level', -40);
export const trigEdge = createParam<TriggerEdge>('trigger.edge', { fallback: 'rising', scope: 'trigger' });
export const trigSafetime = numeric('safetime', 0);
export const trigDelay = numeric('delay', 0);
export const trigPretime = numeric('pretime', 0);
export const trigAcqtime = numeric('acqtime', .005);
export const trigRetrigger = numeric('retrigger', 0);
export const trigRetriggerperiod = numeric('retriggerperiod', 0);
export const trigOut = createParam<string>('trigger.out', { fallback: 'none', scope: 'trigger' });
export const trigOutpolarity = createParam<string>('trigger.outpolarity', { fallback: 'positive', scope: 'trigger' });
/** Probability-of-intercept time reported by the device, in seconds. */
export const trigPoi = numeric('poi', 0);

export const triggerFields = {
  source: { slot: trigSource, id: 'select-trg-source', values: ['bus', 'level', 'freerun', 'external', 'timer'] },
  edge: { slot: trigEdge, id: 'select-trg-edge', values: ['rising', 'falling', 'double'] },
  level: { slot: trigLevel, id: 'input-trg-level', min: -150, max: 30 },
  safetime: { slot: trigSafetime, id: 'input-trg-safetime', min: 0, max: 10 },
  delay: { slot: trigDelay, id: 'input-trg-delay', min: 0, max: 10 },
  pretime: { slot: trigPretime, id: 'input-trg-pretime', min: 0, max: 10 },
  acqtime: { slot: trigAcqtime, id: 'input-trg-acqtime', min: .0005, max: 60 },
  retrigger: { slot: trigRetrigger, id: 'input-trg-retrigger', min: 0, max: 65535, integer: true },
  retriggerperiod: { slot: trigRetriggerperiod, id: 'input-trg-retriggerperiod', min: 0, max: 3600 },
  out: { slot: trigOut, id: 'select-trg-out', values: ['none', 'per_hop', 'per_sweep', 'per_profile'] },
  outpolarity: { slot: trigOutpolarity, id: 'select-trg-outpolarity', values: ['positive', 'negative'] },
} satisfies Record<string, { slot: Param<any>; id: string; values?: string[]; min?: number; max?: number; integer?: boolean }>;
export type TriggerKey = keyof typeof triggerFields;
type Field = { slot: Param<any>; id: string; values?: string[]; min?: number; max?: number; integer?: boolean };
let mode = 'std';
export function triggerMode(): string { return mode; }
export function canCapture(): boolean { return mode === 'std' || mode === 'rta'; }
export function canEditTrigger(key: TriggerKey): boolean { return mode === 'rta' || (mode === 'std' && (key === 'source' || key === 'level' || key === 'edge')); }

export function validTriggerValue(key: TriggerKey, raw: string): string | number | null {
  const field: Field = triggerFields[key];
  if (field.values) return field.values.includes(raw) ? raw : null;
  if (raw.trim() === '') return null;
  const value = Number(raw);
  return Number.isFinite(value) && value >= field.min! && value <= field.max!
    && (!field.integer || Number.isInteger(value)) ? value : null;
}

export function renderTriggerParameters(force = false): void {
  for (const [key, field] of Object.entries(triggerFields)) {
    const input = document.getElementById(field.id) as HTMLInputElement | HTMLSelectElement | null;
    if (!input || (!force && document.activeElement === input)) continue;
    if (!field.slot.pending() && input.dataset.appliedUnknown === 'true') { input.value = ''; continue; }
    const value = mode === 'std' && key === 'source'
      ? (S.swpArmed || S.swpHold ? 'level' : 'bus') : String(field.slot.get());
    // External/API sources are displayed faithfully without adding UI capabilities.
    if (input instanceof HTMLSelectElement && !Array.from(input.options).some((o) => o.value === value)) {
      const option = document.createElement('option');
      option.value = value; option.textContent = value; option.disabled = true; option.dataset.reported = 'true';
      input.append(option);
    }
    input.value = value;
  }
}

export function resetTriggerParameters(): void { resetAll('trigger'); renderTriggerParameters(true); }
export function resetTrigger(): void { resetTriggerParameters(); resetTriggerRuntime(); }
export function rejectTriggerEdit(): void { resetTriggerParameters(); resetTriggerRuntime(); }

export function syncTriggerMode(next: string): void {
  if (next !== mode) { mode = next; resetTriggerParameters(); resetTriggerRuntime(); }
  for (const [key, field] of Object.entries(triggerFields)) {
    const input = document.getElementById(field.id) as HTMLInputElement | HTMLSelectElement | null;
    if (input) input.disabled = !canEditTrigger(key as TriggerKey);
    if (key === 'source' && input instanceof HTMLSelectElement) {
      for (const option of input.options) option.disabled = mode === 'std' && option.value !== 'bus' && option.value !== 'level';
    }
  }
  for (const id of ['btn-trg-capture', 'btn-trg-free', 'btn-trg-from-mkr', 'btn-trg-from-peak']) {
    const button = document.getElementById(id) as HTMLButtonElement | null;
    if (button) button.disabled = !canCapture();
  }
}

/** STATUS is read-only: reflecting another client's source must never issue a command. */
export function confirmTriggerFromStatus(req: Record<string, any>): void {
  const actual = req.trigger_actual ?? {};
  for (const field of Object.values(triggerFields)) {
    const input = document.getElementById(field.id);
    if (input) input.dataset.appliedUnknown = String(mode === 'rta' && actual.valid === false && actual.simulated !== true);
  }
  for (const [key, field] of Object.entries(triggerFields)) {
    if (mode === 'rta' && actual.valid === false && actual.simulated !== true) continue;
    const value = mode === 'rta' && actual[key] != null ? actual[key] : req[`trigger_${key}`];
    if (value == null) continue;
    const parsed = validTriggerValue(key as TriggerKey, String(value));
    if (parsed !== null) (field.slot as Param<any>).confirm(parsed);
  }
  trigPoi.confirm(typeof actual.poi === 'number' && Number.isFinite(actual.poi) ? actual.poi : 0);
  renderTriggerParameters();
}
