// Amplitude units and the external gain/loss offset, applied at the *readout* layer.
//
// Rules:
//  - Absolute trace levels (marker table, peak list, channel power, limit levels) are
//    converted: display = dBm + unit offset + external offset.
//  - Differences (deltas, dBc, margins, dB/div) are unit independent and never converted.
//  - Device parameters (Ref, attenuation) stay in dBm: they describe the instrument, not
//    the signal after the user's cable/amplifier.
//
// Conversions are for a 50 ohm system, which is what the SAN series uses.
import * as S from '../core/store';

export type LevelUnit = 'dBm' | 'dBmV' | 'dBuV' | 'dBV';

export const LEVEL_UNITS: LevelUnit[] = ['dBm', 'dBmV', 'dBuV', 'dBV'];

const UNIT_OFFSET_DB: Record<LevelUnit, number> = {
  dBm: 0,
  dBmV: 46.99,    // 0 dBm = 47 dBmV  (sqrt(0.001 * 50) V -> 223.6 mV)
  dBuV: 106.99,   // 0 dBm = 107 dBuV
  dBV: -13.01,    // 0 dBm = -13.01 dBV
};

export function unitOffsetDb(unit: LevelUnit): number {
  return UNIT_OFFSET_DB[unit] ?? 0;
}

/** Instrument reading (dBm) -> what the user should see, including the external offset. */
export function toDisplayLevel(dbm: number): number {
  return dbm + unitOffsetDb(S.levelUnit) + S.displayOffset;
}

/** What the user typed -> instrument-domain dBm. */
export function fromDisplayLevel(value: number): number {
  return value - unitOffsetDb(S.levelUnit) - S.displayOffset;
}

export function levelUnit(): LevelUnit {
  return S.levelUnit;
}

/** Formatted absolute level in the selected unit, e.g. "-22.46 dBuV". */
export function fmtLevel(dbm: number, digits = 2): string {
  if (!Number.isFinite(dbm)) return '-';
  return `${toDisplayLevel(dbm).toFixed(digits)} ${S.levelUnit}`;
}

/** Wire the unit selector (id="select-level-unit") once at start-up. */
export function initLevelUnit(onChange: () => void): void {
  const sel = document.getElementById('select-level-unit') as HTMLSelectElement | null;
  if (!sel) return;
  sel.value = S.levelUnit;
  sel.addEventListener('change', () => {
    const v = sel.value as LevelUnit;
    S.setLevelUnit(LEVEL_UNITS.includes(v) ? v : 'dBm');
    onChange();
  });
}
