// Unit helpers (from buildUnitGroups/setUnit/parseFreqUnit/toUnit)
import { units } from './store';

export function buildUnitGroups() {
  const defs: Record<string, string[]> = {
    center: ['Hz', 'kHz', 'MHz', 'GHz'], span: ['Hz', 'kHz', 'MHz', 'GHz'],
    start: ['Hz', 'kHz', 'MHz', 'GHz'], stop: ['Hz', 'kHz', 'MHz', 'GHz'],
    rbw: ['Hz', 'kHz', 'MHz'], vbw: ['Hz', 'kHz', 'MHz'], pnm: ['Hz', 'kHz', 'MHz', 'GHz'],
    rta_center: ['MHz', 'GHz'],
  };
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

export function setUnit(field: string, unit: string) {
  const previous = units[field];
  const input = document.getElementById(`input-${field}`) as HTMLInputElement | null;
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
  const el = document.getElementById(`input-${field}`) as HTMLInputElement;
  const v = parseFloat(el?.value || '') || 0;
  const u = units[field];
  return u === 'GHz' ? v * 1e9 : u === 'MHz' ? v * 1e6 : u === 'kHz' ? v * 1e3 : v;
}
export function toUnit(hz: number, field: string): number {
  const u = units[field];
  return u === 'GHz' ? hz / 1e9 : u === 'MHz' ? hz / 1e6 : u === 'kHz' ? hz / 1e3 : hz;
}
