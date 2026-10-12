// Ham S-report mapping (ux-3): IARU HF convention — S9 = -73 dBm, 6 dB per S unit,
// S1 = -121 dBm; above S9 the report is "S9+X dB" capped at +60 (contract: the formula
// is unit-tested on the edges -121/-73/-67/-13).

export function sReport(dbm: number): string {
  if (!Number.isFinite(dbm)) return '-';
  if (dbm >= -73) {
    const over = Math.min(60, Math.round(dbm + 73));
    return over > 0 ? `S9+${over}dB` : 'S9';
  }
  const s = Math.max(0, Math.min(9, Math.round((dbm + 127) / 6)));
  return `S${s}`;
}
