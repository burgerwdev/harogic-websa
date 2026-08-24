// 频谱渲染主模块: 网格/迹线/marker/OSD/3dB/峰标记
import * as S from '../core/store';
import { ctx, W, H, MARGIN } from '../core/store';
import { plotRect } from './plot';
import { canvasColors } from '../core/theme';
import { formatFreqHz, fmtAxis, fmtF } from '../core/fmt';
import { getDisplayPowers } from '../dsp/peaks';
import { smoothForDisplay } from '../dsp/smooth';
import { markerFreqHz } from '../core/markerCommon';
import { autoPeakThr, updatePeakTable, renderPeakMarks, peakListOn } from './peaklist';
import { updateMarkerTable } from './markerTable';
import { updateHarmonicTable } from '../meas/harmonic';
import { renderHarmOverlay } from '../meas/harmOverlay';
import { renderHarmonics } from '../meas/harmOverlay2';
import { renderAmp } from '../meas/amplitude';
import { renderPnm, updatePnmTable } from '../meas/phaseNoise';

// 从 store 取可变引用(模块内部使用快照, 渲染时重新读取)
function cur() {
  return {
    centerHz: S.centerHz, spanHz: S.spanHz, dbPerDiv: S.dbPerDiv, displayRef: S.displayRef,
    displayOffset: S.displayOffset, displayUnit: S.displayUnit, viewMode: S.viewMode,
    measOn: S.measOn, measTabSel: S.measTabSel, traces: S.traces, markers: S.markers,
    activeMkrId: S.activeMkrId, freqArray: S.freqArray, m3dB: S.m3dB, harm: S.harm,
    ampRes: S.ampRes, peakListOn: S.peakListOn, smoothBins: S.smoothBins,
  };
}

export function getY(val: number): number {
  const p = plotRect();
  if (isFinite(val)) val += cur().displayOffset;
  const top = cur().displayRef, bottom = cur().displayRef - S.totalDivs * cur().dbPerDiv;
  if (!isFinite(val)) val = bottom - 10;
  return p.y + ((top - val) / (top - bottom)) * p.h;
}
export function getX(idx: number, points: number): number {
  const p = plotRect();
  return p.x + (idx / (points - 1)) * p.w;
}

export function renderGrid() {
  const c = cur();
  const col = canvasColors();
  ctx.clearRect(0, 0, W, H);
  const p = plotRect();
  ctx.strokeStyle = col.grid; ctx.lineWidth = 1;
  ctx.beginPath();
  for (let i = 1; i < S.totalDivs; i++) {
    const x = p.x + i * p.w / S.totalDivs;
    ctx.moveTo(x, p.y); ctx.lineTo(x, p.y + p.h);
    const y = p.y + i * p.h / S.totalDivs;
    ctx.moveTo(p.x, y); ctx.lineTo(p.x + p.w, y);
  }
  ctx.stroke();
  ctx.strokeStyle = col.axis;
  ctx.strokeRect(p.x, p.y, p.w, p.h);

  ctx.fillStyle = col.axis; ctx.font = '11px monospace';
  ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  const labelX = p.x + p.w + 42;
  for (let i = 0; i <= S.totalDivs; i++) {
    const y = p.y + i * p.h / S.totalDivs;
    const v = c.displayRef - i * c.dbPerDiv;
    ctx.fillText(v.toFixed(0), labelX, y);
  }

  ctx.font = '11px monospace';
  ctx.textBaseline = 'top';
  let fx = p.x;
  const fy = p.y + p.h + 8;
  let loHz = c.centerHz - c.spanHz / 2, hiHz = c.centerHz + c.spanHz / 2;
  if (S.freqArray && S.freqArray.length > 1) { loHz = S.freqArray[0]; hiHz = S.freqArray[S.freqArray.length - 1]; }
  const cHz = (loHz + hiHz) / 2;
  const segs: [string, string][] = [
    ['Start ', formatFreqHz(loHz)],
    ['Stop ', formatFreqHz(hiHz)],
    ['Center ', formatFreqHz(cHz)],
    ['Span ', formatFreqHz(hiHz - loHz)],
  ];
  for (const [l, v] of segs) {
    ctx.textAlign = 'left';
    ctx.fillStyle = col.axis;
    ctx.fillText(l, fx, fy);
    fx += ctx.measureText(l).width;
    ctx.fillStyle = col.text;
    ctx.fillText(v, fx, fy);
    fx += ctx.measureText(v).width + 16;
  }

  if (c.displayUnit === 'dB') {
    ctx.strokeStyle = col.axis;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    const y0 = getY(0);
    ctx.moveTo(p.x, y0); ctx.lineTo(p.x + p.w, y0);
    ctx.stroke();
    ctx.setLineDash([]);
  }
}

export function renderTraceLine(t: S.TraceState) {
  if (!t.powers || t.mode === 'OFF') return;
  const col = canvasColors();
  const p = plotRect();
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  const data = S.smoothBins > 1 ? smoothForDisplay(t.powers, t.mode) : t.powers;
  const n = data.length;
  ctx.strokeStyle = col.traces[t.id - 1] || col.traces[0];
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  let pen = false;
  for (let i = 0; i < n; i++) {
    const v = data[i];
    if (!isFinite(v) || v < -300) { pen = false; continue; }
    const x = getX(i, n), y = getY(v);
    if (!pen) { ctx.moveTo(x, y); pen = true; } else ctx.lineTo(x, y);
  }
  ctx.stroke();
  ctx.restore();
}

// -3dB 测量叠加
function render3dB(powers: Float32Array) {
  const m3 = cur().m3dB;
  if (!m3 || !S.freqArray) return;
  const col = canvasColors();
  const p = plotRect();
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  const yT = getY(m3.thresh);
  ctx.strokeStyle = col.axis;
  ctx.setLineDash([6, 4]);
  ctx.beginPath(); ctx.moveTo(p.x, yT); ctx.lineTo(p.x + p.w, yT); ctx.stroke();
  ctx.setLineDash([]);
  const xL = getX(Math.min(m3.li, powers.length - 1), powers.length);
  const xR = getX(Math.min(m3.ri, powers.length - 1), powers.length);
  ctx.fillStyle = col.text;
  for (const x of [xL, xR]) {
    ctx.beginPath();
    ctx.moveTo(x, yT - 5); ctx.lineTo(x + 4, yT); ctx.lineTo(x, yT + 5); ctx.lineTo(x - 4, yT);
    ctx.closePath(); ctx.fill();
  }
  const yLabel = p.y + 4;
  ctx.fillStyle = col.text;
  ctx.font = 'bold 11px monospace';
  ctx.textAlign = 'left'; ctx.textBaseline = 'top';
  ctx.fillText('-' + (m3.thr || 3) + 'dB BW: ' + formatFreqHz(m3.bw) +
    '  (' + fmtAxis(m3.lf) + ' ~ ' + fmtAxis(m3.rf) + ')', p.x + 4, yLabel);
  ctx.restore();
}

function renderMarkersOnCanvas(powers: Float32Array) {
  const c = cur();
  const col = canvasColors();
  const n = powers.length;
  const p = plotRect();
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  c.markers.forEach((m, mi) => {
    if (!m.enabled || m.mode === 'OFF') return;
    const idx = Math.min(m.idx, n - 1);
    const v = powers[idx];
    const x = getX(idx, n);
    const isActive = m.id === c.activeMkrId;
    const mcol = col.markerColors[mi] || col.marker;
    ctx.strokeStyle = mcol;
    ctx.globalAlpha = isActive ? 0.6 : 0.45;
    ctx.setLineDash([3, 4]);
    ctx.lineWidth = isActive ? 1.6 : 1;
    ctx.beginPath(); ctx.moveTo(x, p.y); ctx.lineTo(x, p.y + p.h); ctx.stroke();
    ctx.setLineDash([]);
    ctx.globalAlpha = 1;
    if (isFinite(v)) {
      const y = getY(v);
      const isV = idx > 0 && idx < n - 1 && v <= powers[idx - 1] && v <= powers[idx + 1];
      const isPk = idx > 0 && idx < n - 1 && v >= powers[idx - 1] && v >= powers[idx + 1];
      const yd = (isV && !isPk) ? y + 15 : y - 15;
      ctx.fillStyle = mcol;
      ctx.beginPath();
      ctx.moveTo(x, yd - 7); ctx.lineTo(x + 5, yd); ctx.lineTo(x, yd + 7); ctx.lineTo(x - 5, yd);
      ctx.closePath(); ctx.fill();
      if (isActive) {
        ctx.strokeStyle = col.text;
        ctx.lineWidth = 1.4;
        ctx.beginPath();
        ctx.moveTo(x, yd - 8); ctx.lineTo(x + 6, yd); ctx.lineTo(x, yd + 8); ctx.lineTo(x - 6, yd);
        ctx.closePath(); ctx.stroke();
        ctx.lineWidth = 1;
      }
      ctx.font = 'bold 11px monospace';
      ctx.textAlign = x > p.x + p.w - 60 ? 'right' : 'left';
      ctx.fillStyle = mcol;
      let ly = yd - 10;
      if (ly < p.y + 8) ly = yd + 14;
      if (isV && !isPk) {
        ly = yd + 18;
        if (ly > p.y + p.h - 4) ly = yd - 12;
      }
      ctx.fillText(`M${m.id}`, x + (x > p.x + p.w - 60 ? -8 : 8), ly);
    }
    if (m.mode === 'DELTA') {
      const ref = c.markers.find(x => x.id === m.refId);
      if (ref && ref.enabled) {
        const xr = getX(Math.min(ref.idx, n - 1), n);
        ctx.strokeStyle = 'rgba(255,255,255,0.35)';
        ctx.setLineDash([2, 5]);
        ctx.beginPath(); ctx.moveTo(xr, p.y); ctx.lineTo(xr, p.y + p.h); ctx.stroke();
        ctx.setLineDash([]);
        drawDimLine(x, xr, p, m, ref);
      }
    }
  });
  ctx.restore();
}

function drawDimLine(x1: number, x2: number, p: { x: number; y: number; w: number; h: number }, m: any, ref: any) {
  const col = canvasColors();
  const xL = Math.min(x1, x2), xR = Math.max(x1, x2);
  const yLine = p.y + 14;
  const df = Math.abs(markerFreqHz(m.idx) - markerFreqHz(ref.idx));
  ctx.save();
  ctx.strokeStyle = col.axis;
  ctx.fillStyle = col.axis;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.beginPath();
  ctx.moveTo(xL, p.y); ctx.lineTo(xL, yLine);
  ctx.moveTo(xR, p.y); ctx.lineTo(xR, yLine);
  ctx.stroke();
  ctx.setLineDash([]);
  ctx.beginPath();
  ctx.moveTo(xL, yLine); ctx.lineTo(xR, yLine);
  ctx.moveTo(xL - 5, yLine); ctx.lineTo(xL + 5, yLine);
  ctx.moveTo(xR - 5, yLine); ctx.lineTo(xR + 5, yLine);
  ctx.stroke();
  const label = 'Δ' + formatFreqHz(df);
  ctx.font = 'bold 10px monospace';
  const tw = ctx.measureText(label).width;
  let cx = (xL + xR) / 2;
  cx = Math.max(p.x + tw / 2 + 2, Math.min(p.x + p.w - tw / 2 - 2, cx));
  const lx = cx - tw / 2 - 2, rx = cx + tw / 2 + 2;
  if (rx - lx > (xR - xL)) {
    cx = Math.min(p.x + p.w - tw / 2 - 2, xR + 12 + tw / 2);
  }
  const ty = yLine - 3;
  ctx.fillStyle = col.labelBg;
  ctx.fillRect(cx - tw / 2 - 3, ty - 8, tw + 6, 11);
  ctx.strokeStyle = col.labelBorder;
  ctx.lineWidth = 0.8;
  ctx.strokeRect(cx - tw / 2 - 3, ty - 8, tw + 6, 11);
  ctx.fillStyle = col.text;
  ctx.textAlign = 'center'; ctx.textBaseline = 'bottom';
  ctx.fillText(label, cx, ty);
  ctx.restore();
}

function renderOSD(powers: Float32Array) {
  const c = cur();
  const col = canvasColors();
  const active = c.markers.filter(m => m.enabled && m.mode !== 'OFF');
  if (!active.length) return;
  const unit = c.displayUnit === 'dB' ? 'dB' : 'dBm';
  const p = plotRect();
  ctx.font = '11px monospace';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'top';
  let line = 0;
  active.forEach(m => {
    const cy = p.y + 28 + line * 14;
    const idx = Math.min(m.idx, powers.length - 1);
    const f = markerFreqHz(idx), a = powers[idx];
    let txt: string;
    if (m.mode === 'NORMAL') {
      txt = `M${m.id} ${formatFreqHz(f)}  ${a.toFixed(2)}${unit}`;
    } else if (m.mode === 'DELTA') {
      const ref = c.markers.find(x => x.id === m.refId);
      if (ref) {
        const rf = markerFreqHz(Math.min(ref.idx, powers.length - 1));
        const ra = powers[Math.min(ref.idx, powers.length - 1)];
        txt = `M${m.id} Δ${formatFreqHz(Math.abs(f - rf))}  Δ${(a - ra).toFixed(2)}dB`;
      } else txt = `M${m.id}`;
    } else txt = `M${m.id}`;
    ctx.strokeStyle = col.osdStroke;
    ctx.lineWidth = 3;
    ctx.strokeText(txt, p.x + 8, cy);
    ctx.fillStyle = col.osdFill;
    ctx.fillText(txt, p.x + 8, cy);
    line++;
  });
}

export function renderAll() {
  const c = cur();
  if (c.viewMode === 'pnm') { renderPnm(); updatePnmTable(); return; }
  if (c.viewMode === 'harm') {
    renderGrid();
    c.traces.forEach(t => renderTraceLine(t));
    const powers2 = getDisplayPowers();
    if (powers2 && S.freqArray) renderHarmOverlay(powers2);
    updateHarmonicTable();
    return;
  }
  renderGrid();
  c.traces.forEach(t => renderTraceLine(t));
  const powers = getDisplayPowers();
  if (powers && S.freqArray) {
    if (c.viewMode !== 'harm' && c.viewMode !== 'pnm') {
      renderMarkersOnCanvas(powers);
      renderOSD(powers);
      render3dB(powers);
    }
    if (c.viewMode === 'harm') renderHarmonics(powers);
    if (c.measOn && c.measTabSel === 'amp') renderAmp(powers);
  }
  if (powers) {
    autoPeakThr(powers);
    if (peakListOn() && c.viewMode === 'std') {
      const mt = document.getElementById('marker-table');
      if (mt) mt.style.display = 'none';
      updatePeakTable(powers);
      renderPeakMarks(powers);
    } else {
      const mt = document.getElementById('marker-table');
      if (mt) mt.style.display = '';
      const pt = document.getElementById('peak-table');
      if (pt) pt.style.display = 'none';
      updateMarkerTable(powers);
    }
    if (c.viewMode === 'harm') updateHarmonicTable();
  }
}
