// Unit helpers (from buildUnitGroups/setUnit/parseFreqUnit/toUnit)
import { units } from './store';

/** Fields that carry a unit button group, with the units each one accepts. */
export const UNIT_OPTIONS: Record<string, string[]> = {
  center: ['Hz', 'kHz', 'MHz', 'GHz'], span: ['Hz', 'kHz', 'MHz', 'GHz'],
  start: ['Hz', 'kHz', 'MHz', 'GHz'], stop: ['Hz', 'kHz', 'MHz', 'GHz'],
  rbw: ['Hz', 'kHz', 'MHz'], vbw: ['Hz', 'kHz', 'MHz'], pnm: ['Hz', 'kHz', 'MHz', 'GHz'],
  rta_center: ['MHz', 'GHz'],
};

export function buildUnitGroups() {
  const defs = UNIT_OPTIONS;
  for (const [f, opts] of Object.entries(defs)) {
    const grp = document.getElementById(`unit-${f}-group`);
    if (!grp) continue;
    grp.innerHTML = '';
    for (const u of opts) {
      const b = document.createElement('button');
      b.className = 'unit-btn' + (units[f] === u ? ' active' : '');
      b.textContent = u;
      b.onclick = () => setUnit(f, u);
      grp.appendChild(b);
    }
  }
}
export interface UnitCommitDetail {
  field: string;
  unit: string;
  commit: boolean;
}

function unitScale(unit: string): number {
  return unit === 'GHz' ? 1e9 : unit === 'MHz' ? 1e6 : unit === 'kHz' ? 1e3 : 1;
}

export function convertUnitValue(value: number, from: string, to: string): number {
  return value * unitScale(from) / unitScale(to);
}

function formatUnitValue(value: number, unit: string): string {
  const digits = unit === 'Hz' ? 0 : unit === 'kHz' ? 3 : unit === 'MHz' ? 4 : 6;
  return value.toFixed(digits);
}

/**
 * Input element for a unit-group key.
 *
 * Unit groups are keyed with an underscore (`rta_center`, matching `units[]`) while the
 * input ids use a hyphen (`input-rta-center`). Looking up `input-rta_center` silently found
 * nothing, so the unit buttons neither converted the value nor committed it, and the virtual
 * keypad could not resolve the field (no unit row, OK did nothing).
 */
export function inputForField(field: string): HTMLInputElement | null {
  for (const id of [`input-${field}`, `input-${field.replace(/_/g, '-')}`]) {
    const el = document.getElementById(id);
    if (el) return el as HTMLInputElement;
  }
  return null;
}

/** Canonical units[] key for an input element (`input-rta-center` -> `rta_center`), or ''. */
export function fieldForInput(input: { id: string }): string {
  const key = input.id.startsWith('input-') ? input.id.slice('input-'.length) : '';
  if (!key) return '';
  if (UNIT_OPTIONS[key]) return key;
  const underscored = key.replace(/-/g, '_');
  return UNIT_OPTIONS[underscored] ? underscored : '';
}

export function setUnit(field: string, unit: string) {
  const previous = units[field];
  const input = inputForField(field);
  const edited = input?.dataset.edited === '1';
  if (input && !edited) {
    const value = Number(input.value);
    if (isFinite(value)) input.value = formatUnitValue(
      convertUnitValue(value, previous, unit), unit);
  }
  units[field] = unit;
  const grp = document.getElementById(`unit-${field}-group`);
  if (grp) for (const b of grp.children) (b as HTMLElement).classList.toggle('active', b.textContent === unit);
  document.dispatchEvent(new CustomEvent<UnitCommitDetail>(
    'websa:unit-commit', { detail: { field, unit, commit: edited } }));
}
export function parseFreqUnit(field: string): number {
  const el = inputForField(field);
  const v = parseFloat(el?.value || '') || 0;
  const u = units[field];
  return u === 'GHz' ? v * 1e9 : u === 'MHz' ? v * 1e6 : u === 'kHz' ? v * 1e3 : v;
}
export function toUnit(hz: number, field: string): number {
  const u = units[field];
  return u === 'GHz' ? hz / 1e9 : u === 'MHz' ? hz / 1e6 : u === 'kHz' ? hz / 1e3 : hz;
}
