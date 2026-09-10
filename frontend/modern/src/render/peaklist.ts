// Pk list: peak detection/threshold/table/on-canvas marks
import * as S from '../core/store';
import { getX, getY } from './spectrum';
import { fmtF } from '../core/fmt';
import { t } from '../core/i18n';

export function peakListOn(): boolean { return S.peakListOn; }

export function togglePeakList() {
  S.setPeakListOn(!S.peakListOn);
  const btn = document.getElementById('btn-peaklist');
  if (btn) btn.textContent = S.peakListOn ? t('on') : t('off');
  if (S.peakListOn) renderAll();
  renderAll();
}

export function findPeaks(powers: Float32Array, maxN: number): { idx: number; amp: number; f: number }[] {
  const el = document.getElementById('input-peakthr') as HTMLInputElement;
  const thr = el ? (parseFloat(el.value) || -80) : -80;
  const fa = S.freqArray;
  const peaks: { idx: number; amp: number; f: number }[] = [];
  for (let i = 1; i < powers.length - 1; i++) {
    const v = powers[i];
    if (v > thr && v >= powers[i - 1] && v > powers[i + 1]) {
      peaks.push({ idx: i, amp: v, f: fa ? fa[i] : i });
    }
  }
  peaks.sort((a, b) => a.idx - b.idx);
  const out: typeof peaks = [];
  for (const pk of peaks) {
    let replaced = false;
    for (let j = 0; j < out.length; j++) {
      if (Math.abs(out[j].idx - pk.idx) < 8 && Math.abs(out[j].amp - pk.amp) < 10) {
        if (pk.amp > out[j].amp) out[j] = pk;
        replaced = true; break;
      }
    }
    if (!replaced) {
      out.push(pk);
      if (out.length >= maxN) break;
    }
  }
  return out;
}

export function autoPeakThr(powers: Float32Array | null) {
  if (S.peakThrUserSet) return;
  if (!powers) return;
  if (!S.markers.some(m => m.enabled)) return;
  const el = document.getElementById('input-peakthr') as HTMLInputElement;
  if (!el) return;
  if (document.activeElement === el) return;
  let peak = -1e9;
  for (const v of powers) if (v > peak) peak = v;
  if (peak > -200) {
    const thr = Math.round(peak - 50);
    if (el.value !== String(thr)) el.value = String(thr);
  }
}

export function peakThrManual() { S.setPeakThrUserSet(true); renderAll(); }
export function peakThrAuto() {
  S.setPeakThrUserSet(false);
  const dp = getDisplayPowers();
  if (dp) {
    let peak = -1e9;
    for (const v of dp) if (v > peak) peak = v;
    const el = document.getElementById('input-peakthr') as HTMLInputElement;
    if (el && peak > -200) el.value = String(Math.round(peak - 50));
  }
  renderAll();
}

export function updatePeakTable(powers: Float32Array | null) {
  const tb = document.getElementById('peak-table');
  const tb2 = document.getElementById('peak-tbody');
  if (!tb || !tb2) return;
  if (!S.peakListOn || !powers || !S.freqArray) { tb.style.display = 'none'; return; }
  const cntEl = document.getElementById('input-peakcnt') as HTMLInputElement;
  const maxN = Math.max(1, Math.min(20, cntEl ? (parseInt(cntEl.value) || 20) : 20));
  const peaks = findPeaks(powers, maxN);
  S.setPeakMarks(peaks.map((pk, i) => Object.assign({}, pk, { n: i + 1 })));
  tb.style.display = '';
  const maxCols = 5, maxRows = 4;
  let html = '';
  for (let r = 0; r < maxRows; r++) {
    let cells = '';
    for (let c = 0; c < maxCols; c++) {
      const idx = r * maxCols + c;
      const pk = idx < peaks.length ? peaks[idx] : null;
      if (!pk) continue;
      cells += '<td class="pk-cell" title="Peak P' + (idx + 1) + '">' +
        '<span class="pk-id">P' + (idx + 1) + ':</span> ' +
        '<span class="pk-f">' + fmtF(pk.f) + '</span> / ' +
        '<span class="pk-a">' + pk.amp.toFixed(1) + ' dBm</span></td>';
    }
    if (cells) html += '<tr>' + cells + '</tr>';
  }
  tb2.innerHTML = html;
}

export function renderPeakMarks(powers: Float32Array) {
  if (!S.peakListOn || !S.peakMarks || !S.freqArray) return;
  const col = getColors();
  const p = plotRectLocal();
  const n = powers.length;
  ctx2.save();
  ctx2.beginPath(); ctx2.rect(p.x, p.y, p.w, p.h); ctx2.clip();
  ctx2.font = 'bold 9px monospace';
  S.peakMarks.forEach((pk) => {
    if (pk.idx < 0 || pk.idx >= n) return;
    const x = getX(pk.idx, n);
    const y = getY(pk.amp);
    const yd = y - 10;
    ctx2.fillStyle = col.peak;
    ctx2.beginPath();
    ctx2.moveTo(x, yd - 4); ctx2.lineTo(x + 3, yd); ctx2.lineTo(x, yd + 4); ctx2.lineTo(x - 3, yd);
    ctx2.closePath(); ctx2.fill();
    const lbl = 'P' + pk.n;
    const tw = ctx2.measureText(lbl).width;
    let ly = yd - 6;
    if (ly < p.y + 8) ly = yd + 10;
    ctx2.fillStyle = col.labelBg;
    ctx2.fillRect(x - tw / 2 - 2, ly - 7, tw + 4, 10);
    ctx2.fillStyle = col.peak;
    ctx2.textAlign = 'center'; ctx2.textBaseline = 'bottom';
    ctx2.fillText(lbl, x, ly);
  });
  ctx2.restore();
}

import { ctx as ctx2, W as _W, H as _H, MARGIN as _M } from '../core/store';
import { plotRect as plotRectLocal } from './plot';
import { canvasColors as getColors } from '../core/theme';
import { getDisplayPowers } from '../dsp/peaks';
import { renderAll } from './spectrum';
