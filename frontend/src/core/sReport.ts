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

// S-scale bar geometry (ux-3): the bar is divided into 10 even segments — S1..S9 (the 9
// IARU units, -121..-73 dBm) and S9+60 (the -73..-13 dBm over range) — so a ham reads the
// fill against the same non-linear scale as an S meter. Returns the fill position in %.
const S_SCALE_POINTS: Array<[number, number]> = [
  [-121, 0], [-115, 10], [-109, 20], [-103, 30], [-97, 40], [-91, 50],
  [-85, 60], [-79, 70], [-73, 80],
  // the over-range (+0..+60 dB) shares the last fifth, +10 dB per 3.33%
  [-63, 83.333], [-53, 86.667], [-43, 90], [-33, 93.333], [-23, 96.667], [-13, 100],
];

export function sScalePct(dbm: number): number {
  if (!Number.isFinite(dbm)) return 0;
  if (dbm <= S_SCALE_POINTS[0][0]) return 0;
  const last = S_SCALE_POINTS[S_SCALE_POINTS.length - 1];
  if (dbm >= last[0]) return 100;
  for (let i = 1; i < S_SCALE_POINTS.length; i++) {
    const [hiDbm, hiPct] = S_SCALE_POINTS[i];
    if (dbm <= hiDbm) {
      const [loDbm, loPct] = S_SCALE_POINTS[i - 1];
      return loPct + (hiPct - loPct) * (dbm - loDbm) / (hiDbm - loDbm);
    }
  }
  return 100;
}

/** The bar's tick labels: unit boundaries every other segment plus the +dB range ends. */
export const S_SCALE_TICKS: Array<{ pct: number; label: string }> = [
  { pct: 0, label: 'S1' }, { pct: 20, label: 'S3' }, { pct: 40, label: 'S5' },
  { pct: 60, label: 'S7' }, { pct: 80, label: 'S9' }, { pct: 90, label: '+30' },
  { pct: 100, label: '+60' },
];
