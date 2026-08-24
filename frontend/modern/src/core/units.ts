// 单位组工具 (原 buildUnitGroups/setUnit/parseFreqUnit/toUnit)
import { units } from './store';

export function buildUnitGroups() {
  const defs: Record<string, string[]> = {
    center: ['Hz', 'kHz', 'MHz', 'GHz'], span: ['Hz', 'kHz', 'MHz', 'GHz'],
    start: ['Hz', 'kHz', 'MHz', 'GHz'], stop: ['Hz', 'kHz', 'MHz', 'GHz'],
    rbw: ['Hz', 'kHz', 'MHz'], vbw: ['Hz', 'kHz', 'MHz'], pnm: ['Hz', 'kHz', 'MHz', 'GHz'],
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
export function setUnit(field: string, unit: string) {
  units[field] = unit;
  const grp = document.getElementById(`unit-${field}-group`);
  if (grp) for (const b of grp.children) (b as HTMLElement).classList.toggle('active', b.textContent === unit);
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
