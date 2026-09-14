// Harmonic measurement (server auto-tunes H1~H5) + table
import * as S from '../core/store';
import { formatFreqHz } from '../core/fmt';
import { t } from '../core/i18n';
import { renderHarmOverlay } from './harmOverlay';
import { registerViewRenderer } from '../render/registry';
import { harmValMode } from '../ui/measurePrefs';
import { requestRender } from '../render/redraw';
import { registerMeasurementTab } from '../ui/measureRegistry';

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
  if (harmValMode.get() === 'Peak') {
    for (let i = 0; i < list.length; i++) {
      const cur = a.peak[i];
      if (!cur || list[i].amp > cur.amp) a.peak[i] = Object.assign({}, list[i]);
    }
  } else if (harmValMode.get() === 'Avg') {
    for (let i = 0; i < list.length; i++) {
      if (!a.sum[i]) a.sum[i] = 0;
      a.sum[i] += list[i].amp;
    }
    a.cnt++;
  } else if (harmValMode.get() === 'Frz') {
    if (!a.frozen) a.frozen = list.map((x: any) => Object.assign({}, x));
  } else {
    a.cnt++;
  }
  const disp = buildHarmDisplay();
  if (disp) { S.setHarm(disp); requestRender(); }
}

function buildHarmDisplay(): any {
  const last = S.lastHarmList;
  if (!last) return null;
  const a = S.harmAccum;
  let list: any[];
  if (harmValMode.get() === 'RT') list = last;
  else if (harmValMode.get() === 'Peak') list = a.peak;
  else if (harmValMode.get() === 'Frz') list = a.frozen || last;
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

import { send } from '../core/wsSend';
import { applyMeasUI } from '../ui/measureUi';

// Register this view with the renderer hub (report finding E-5). The hub passes the swept
// drawing primitives in, so this module never imports render/spectrum.ts back.
registerViewRenderer({
  mode: 'harm',
  render: (ctx) => {
    ctx.renderGrid();
    S.traces.forEach(t => ctx.renderTraceLine(t));
    const powers = ctx.getDisplayPowers();
    if (powers && S.freqArray) renderHarmOverlay(powers);
    updateHarmonicTable();
  },
});

// Register this measurement tab with the registry (report finding E-5).
registerMeasurementTab({ id: 'harm', domId: 'tab-harm', apply: measHarmApply, updateTable: updateHarmonicTable });
