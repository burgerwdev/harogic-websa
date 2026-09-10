// Harmonic measurement (server auto-tunes H1~H5) + table
import * as S from '../core/store';
import { formatFreqHz } from '../core/fmt';
import { t } from '../core/i18n';
import { renderAll } from '../render/spectrum';

export function measureHarmonics() {
  const dp = getDisplayPowers();
  if (!dp || !S.freqArray || S.freqArray.length < 2) { alert(t('meas_no_trace')); return; }
  const inp = document.getElementById('input-harm') as HTMLInputElement;
  const count = parseInt(inp?.value || '5') || 5;
  const nPts = S.freqArray.length;
  let pi = 0, pv = -1e9;
  for (let i = 0; i < dp.length; i++) if (dp[i] > pv) { pv = dp[i]; pi = i; }
  const f0 = S.freqArray[pi];
  if (pv <= -1e8) { alert(t('meas_no_signal')); return; }
  const spanHz = S.freqArray[nPts - 1] - S.freqArray[0];
  const list: { n: number; f: number; amp: number | null; dbc: number | null; idx: number; inSpan: boolean }[] = [];
  const fa = S.freqArray;
  for (let n = 1; n <= count; n++) {
    const f = f0 * n;
    if (f > S.FREQ_MAX) break;
    const fLo = fa[0], fHi = fa[nPts - 1];
    if (f < fLo || f > fHi) {
      list.push({ n, f, amp: null, dbc: null, idx: -1, inSpan: false });
      continue;
    }
    const win = Math.max(spanHz / 40, f * 0.02);
    let i0 = 0, i1 = nPts - 1;
    for (let i = 0; i < nPts; i++) { if (fa[i] >= f - win && i0 === 0) i0 = i; if (fa[i] > f + win) { i1 = i - 1; break; } }
    i0 = Math.max(0, Math.min(i0, nPts - 1)); i1 = Math.max(i0, Math.min(i1, nPts - 1));
    let mi = i0, mv = -1e9;
    for (let i = i0; i <= i1; i++) if (dp[i] > mv) { mv = dp[i]; mi = i; }
    list.push({ n, f: fa[mi], amp: mv, dbc: mv - pv, idx: mi, inSpan: true });
  }
  S.setHarm({ f0, p0: pv, count, list });
  renderAll();
}

export function clearHarmonics() { S.setHarm(null); renderAll(); }

export function autoHarmSpan() {
  const sel = document.getElementById('select-harm-span') as HTMLSelectElement;
  if (!sel) return;
  const f0 = parseFloat((document.getElementById('input-harm-f0') as HTMLInputElement).value) * 1e6 || 1e9;
  const target = f0 / 100;
  const opts = [...sel.options].map(o => parseFloat(o.value));
  let best = opts[0], bd = 1e30;
  for (const v of opts) {
    const d = Math.abs(Math.log10(v / target));
    if (d < bd) { bd = d; best = v; }
  }
  sel.value = String(best);
}

export function measHarmApply() {
  if (!S.measOn || S.measTabSel !== 'harm') return;
  const f0 = parseFloat((document.getElementById('input-harm-f0') as HTMLInputElement).value) * 1e6 || 1e9;
  const count = parseInt((document.getElementById('input-harm-count') as HTMLInputElement).value) || 5;
  const span = parseFloat((document.getElementById('select-harm-span') as HTMLSelectElement).value) || 10e6;
  if (S.viewMode !== 'harm') {
    S.setViewMode('harm');
    send({ cmd: 'SET_MODE', mode: 'harmonic' });
  }
  send({ cmd: 'SET_HARM', f0, count, span });
  S.setHarmAccum(null);
  S.setLastHarmList(null);
  applyMeasUI();
}

export function onHarmResult(list: any[]) {
  if (!list || !list.length) return;
  S.setLastHarmList(list);
  if (!S.harmAccum) S.setHarmAccum({ peak: [], sum: [], cnt: 0, frozen: null });
  const a = S.harmAccum;
  if (S.harmValMode === 'Peak') {
    for (let i = 0; i < list.length; i++) {
      const cur = a.peak[i];
      if (!cur || list[i].amp > cur.amp) a.peak[i] = Object.assign({}, list[i]);
    }
  } else if (S.harmValMode === 'Avg') {
    for (let i = 0; i < list.length; i++) {
      if (!a.sum[i]) a.sum[i] = 0;
      a.sum[i] += list[i].amp;
    }
    a.cnt++;
  } else if (S.harmValMode === 'Frz') {
    if (!a.frozen) a.frozen = list.map((x: any) => Object.assign({}, x));
  } else {
    a.cnt++;
  }
  const disp = buildHarmDisplay();
  if (disp) { S.setHarm(disp); renderAll(); }
}

function buildHarmDisplay(): any {
  const last = S.lastHarmList;
  if (!last) return null;
  const a = S.harmAccum;
  let list: any[];
  if (S.harmValMode === 'RT') list = last;
  else if (S.harmValMode === 'Peak') list = a.peak;
  else if (S.harmValMode === 'Frz') list = a.frozen || last;
  else list = a.sum.map((s: number, i: number) => {
    const o = last[i]; if (!o) return null;
    const am = a.cnt ? s / a.cnt : o.amp;
    return { n: o.n, f: o.f, amp: am, dbc: 0 };
  }).filter(Boolean);
  if (!list || !list.length) return null;
  const base = list[0].amp;
  list = list.map((x: any) => Object.assign({}, x, { dbc: x.amp - base }));
  return { f0: list[0].f, p0: base, list };
}

export function updateHarmonicTable() {
  const tb = document.getElementById('harmonic-table');
  const tb2 = document.getElementById('harmonic-tbody');
  if (!tb || !tb2) return;
  const h = S.harm;
  if (!h || !h.list.length) {
    // Measurement mode is on but no result yet: show the measuring state (rather than hiding)
    if (S.measOn && S.viewMode === 'harm') {
      tb.style.display = '';
      tb2.innerHTML = '<tr><td class="harm-cell">' + t('measuring') + '</td></tr>';
    } else {
      tb.style.display = 'none';
    }
    return;
  }
  tb.style.display = '';
  const perRow = 5;
  let rows: string[] = [];
  for (let r = 0; r < h.list.length; r += perRow) {
    const cells = h.list.slice(r, r + perRow).map((hh) => {
      const amp = isFinite(hh.amp as number) ? (hh.amp as number).toFixed(1) + ' dBm' : '—';
      const dbc = isFinite(hh.dbc as number) ? (hh.dbc as number).toFixed(1) + ' dBc' : '—';
      return '<td class="harm-cell" title="H' + hh.n + ' @ ' + formatFreqHz(hh.f) + '">' +
        '<span class="harm-h">H' + hh.n + '</span> ' + formatFreqHz(hh.f) +
        '<br>' + amp + ' <span class="harm-dbc">' + dbc + '</span></td>';
    });
    rows.push('<tr>' + cells.join('') + '</tr>');
  }
  tb2.innerHTML = rows.join('');
}

import { getDisplayPowers } from '../dsp/peaks';
import { send } from '../core/wsSend';
import { applyMeasUI } from '../ui/measure';
