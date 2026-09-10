// Channel measurements: channel power, occupied bandwidth (OBW) and ACPR.
//
// Everything is computed from the displayed swept trace (no device round-trip), so the
// numbers always match what the user sees. Results are shown in the channel table and
// the channel / adjacent / OBW bands are shaded over the plot.
import * as S from '../core/store';
import { getDisplayPowers } from '../dsp/peaks';
import { acpr, occupiedBandwidth } from '../dsp/channel';
import { getX, renderAll } from '../render/spectrum';
import { plotRect } from '../render/plot';
import { canvasColors } from '../core/theme';
import { fmtF } from '../core/fmt';
import { t } from '../core/i18n';

function num(id: string, dflt: number): number {
  const el = document.getElementById(id) as HTMLInputElement | null;
  const v = parseFloat(el?.value ?? '');
  return Number.isFinite(v) ? v : dflt;
}

/** Channel centre: marker 1 when it is on, otherwise the sweep centre. */
function defaultCenterHz(): number {
  const m = S.markers[0];
  if (m && m.enabled && m.freq) return m.freq;
  return S.centerHz;
}

export function measureChannel(): void {
  const dp = getDisplayPowers();
  const fa = S.freqArray;
  if (!dp || !fa || fa.length < 2) return;
  const centerEl = document.getElementById('input-chan-center') as HTMLInputElement | null;
  if (centerEl && !centerEl.value.trim()) centerEl.value = (defaultCenterHz() / 1e6).toFixed(4);
  const center = num('input-chan-center', defaultCenterHz() / 1e6) * 1e6;
  const chBw = Math.max(1e3, num('input-chan-bw', 1) * 1e6);
  const pct = num('select-chan-obw', 99);
  const offset = Math.max(0, num('input-chan-offset', 2) * 1e6);
  const adjBw = Math.max(1e3, num('input-chan-adjbw', 1) * 1e6);
  const n = Math.min(dp.length, fa.length);
  const res = acpr(fa, dp, { centerHz: center, channelBw: chBw, acpOffset: offset, acpBw: adjBw }, n);
  const obw = occupiedBandwidth(fa, dp, center, pct, n);
  S.setChanRes({
    centerHz: center, channelBw: chBw, obwPercent: pct, acpOffset: offset, acpBw: adjBw,
    mainDbm: res?.mainDbm ?? null,
    lowerDbc: res?.lowerDbc ?? null,
    upperDbc: res?.upperDbc ?? null,
    obw: obw?.bw ?? null,
    obwLow: obw?.low ?? null,
    obwHigh: obw?.high ?? null,
  });
  updateChanTable();
  renderAll();
}

export function clearChannel(): void {
  S.setChanRes(null);
  updateChanTable();
  renderAll();
}

/** Visibility is owned here so the tab machinery and the render loop cannot disagree. */
export function syncChanTableVisibility(): void {
  const tbl = document.getElementById('chan-table');
  if (tbl) tbl.style.display = (S.measOn && S.measTabSel === 'chan') ? '' : 'none';
}

let lastTableKey = '';

/** Fill the channel table; only touches the DOM when the content changes. */
export function updateChanTable(): void {
  const tb = document.getElementById('chan-tbody');
  const tbl = document.getElementById('chan-table');
  if (!tb || !tbl) return;
  const r = S.chanRes;
  const cell = (a: string, b: string) => `<tr><td>${a}</td><td>${b}</td></tr>`;
  const rows: string[] = [];
  if (!r) {
    rows.push(cell(t('chan_power'), '-'));
    rows.push(cell(t('chan_obw'), '-'));
    rows.push(cell(t('chan_acp_l'), '-'));
    rows.push(cell(t('chan_acp_u'), '-'));
  } else {
    rows.push(cell(`${t('chan_power')} (${fmtF(r.channelBw)})`,
      r.mainDbm === null ? '-' : `${r.mainDbm.toFixed(2)} dBm`));
    rows.push(cell(`OBW ${r.obwPercent}%`,
      r.obw === null || r.obwLow === null || r.obwHigh === null
        ? '-'
        : `${fmtF(r.obw)} (${fmtF(r.obwLow)} … ${fmtF(r.obwHigh)})`));
    rows.push(cell(`${t('chan_acp_l')} -${fmtF(r.acpOffset)}`,
      r.lowerDbc === null ? '-' : `${r.lowerDbc.toFixed(2)} dBc`));
    rows.push(cell(`${t('chan_acp_u')} +${fmtF(r.acpOffset)}`,
      r.upperDbc === null ? '-' : `${r.upperDbc.toFixed(2)} dBc`));
  }
  const html = rows.join('');
  const key = `${document.documentElement.lang}|${html}`;   // language change must rebuild the text
  if (key === lastTableKey) return;
  lastTableKey = key;
  tb.innerHTML = html;
}

/** Frequency -> plot x (the frequency axis may be non-uniform, so interpolate). */
function freqToX(f: number): number {
  const fa = S.freqArray;
  if (!fa || fa.length < 2) return plotRect().x;
  const n = fa.length;
  let i1 = n - 1;
  for (let i = 0; i < n; i++) {
    if (fa[i] >= f) { i1 = i; break; }
  }
  const i0 = i1 > 0 ? i1 - 1 : 0;
  if (fa[i1] === fa[i0]) return getX(i0, n);
  return getX(i0 + ((f - fa[i0]) / (fa[i1] - fa[i0])) * (i1 - i0), n);
}

/** Shade the channel and adjacent bands and mark the OBW edges. */
export function renderChannel(_powers: Float32Array): void {
  const r = S.chanRes;
  if (!r) return;
  const p = plotRect();
  const c = S.ctx;
  const col = canvasColors();
  c.save();
  c.beginPath();
  c.rect(p.x, p.y, p.w, p.h);
  c.clip();
  // Main channel band
  let x1 = freqToX(r.centerHz - r.channelBw / 2);
  let x2 = freqToX(r.centerHz + r.channelBw / 2);
  c.fillStyle = 'rgba(0,170,255,0.10)';
  c.fillRect(x1, p.y, Math.max(1, x2 - x1), p.h);
  // Adjacent channel bands
  if (r.acpOffset > 0) {
    c.fillStyle = 'rgba(255,140,0,0.10)';
    x1 = freqToX(r.centerHz - r.acpOffset - r.acpBw / 2);
    x2 = freqToX(r.centerHz - r.acpOffset + r.acpBw / 2);
    c.fillRect(x1, p.y, Math.max(1, x2 - x1), p.h);
    x1 = freqToX(r.centerHz + r.acpOffset - r.acpBw / 2);
    x2 = freqToX(r.centerHz + r.acpOffset + r.acpBw / 2);
    c.fillRect(x1, p.y, Math.max(1, x2 - x1), p.h);
  }
  // OBW edges + label
  if (r.obw !== null && r.obwLow !== null && r.obwHigh !== null) {
    const xl = freqToX(r.obwLow);
    const xr = freqToX(r.obwHigh);
    c.strokeStyle = '#00e5ff';
    c.lineWidth = 1.5;
    c.setLineDash([4, 4]);
    c.beginPath();
    c.moveTo(xl, p.y);
    c.lineTo(xl, p.y + p.h);
    c.moveTo(xr, p.y);
    c.lineTo(xr, p.y + p.h);
    c.stroke();
    c.setLineDash([]);
    const lbl = `OBW ${r.obwPercent}%: ${fmtF(r.obw)}`;
    c.font = 'bold 10px monospace';
    const tw = c.measureText(lbl).width;
    const cx = Math.min(Math.max((xl + xr) / 2, p.x + tw / 2 + 4), p.x + p.w - tw / 2 - 4);
    c.fillStyle = col.labelBg;
    c.fillRect(cx - tw / 2 - 3, p.y + 3, tw + 6, 12);
    c.fillStyle = '#00e5ff';
    c.textAlign = 'center';
    c.textBaseline = 'bottom';
    c.fillText(lbl, cx, p.y + 14);
  }
  c.restore();
}
