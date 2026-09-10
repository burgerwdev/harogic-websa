// Spectrum rendering main module: grid/traces/markers/OSD/3dB/peak marks
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
import { renderWaterfall, pushSwpRow } from './waterfall';
import { buildLimitArray, evaluateAgainst, violationRuns } from '../dsp/limits';
import { updateLimitStatus } from '../ui/limits';

// Take mutable references from the store (snapshot at module level, re-read during render)
function cur() {
  return {
    centerHz: S.centerHz, spanHz: S.spanHz, dbPerDiv: S.dbPerDiv, displayRef: S.displayRef,
    displayOffset: S.displayOffset, displayUnit: S.displayUnit, viewMode: S.viewMode,
    measOn: S.measOn, measTabSel: S.measTabSel, traces: S.traces, markers: S.markers,
    activeMkrId: S.activeMkrId, freqArray: S.freqArray, m3dB: S.m3dB, harm: S.harm,
    ampRes: S.ampRes, peakListOn: S.peakListOn, smoothBins: S.smoothBins,
  };
}

// Fixed plot rectangle: W/H/MARGIN are constants, so this is computed once instead of
// allocating a new object for every point during trace rendering.
const PLOT_RECT = {
  x: MARGIN.left,
  y: MARGIN.top,
  w: W - MARGIN.left - MARGIN.right,
  h: H - MARGIN.top - MARGIN.bottom,
};

export function getY(val: number): number {
  if (isFinite(val)) val += S.displayOffset;
  const top = S.displayRef, bottom = S.displayRef - S.totalDivs * S.dbPerDiv;
  if (!isFinite(val)) val = bottom - 10;
  return PLOT_RECT.y + ((top - val) / (top - bottom)) * PLOT_RECT.h;
}
export function getX(idx: number, points: number): number {
  return PLOT_RECT.x + (idx / (points - 1)) * PLOT_RECT.w;
}

// Shared bottom frequency row (used by both SWP grid and RTA view)
function drawFreqRow(lo: number, hi: number, col: any, p: any) {
  ctx.font = '11px monospace';
  ctx.textBaseline = 'top';
  ctx.textAlign = 'left';
  let fx = p.x;
  const fy = p.y + p.h + 8;
  const cHz = (lo + hi) / 2;
  const segs: [string, string][] = [
    ['Start ', formatFreqHz(lo)],
    ['Stop ', formatFreqHz(hi)],
    ['Center ', formatFreqHz(cHz)],
    ['Span ', formatFreqHz(hi - lo)],
  ];
  for (const [l, v] of segs) {
    ctx.fillStyle = col.axis;
    ctx.fillText(l, fx, fy);
    fx += ctx.measureText(l).width;
    ctx.fillStyle = col.text;
    ctx.fillText(v, fx, fy);
    fx += ctx.measureText(v).width + 16;
  }
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

  let loHz = c.centerHz - c.spanHz / 2, hiHz = c.centerHz + c.spanHz / 2;
  if (S.freqArray && S.freqArray.length > 1) { loHz = S.freqArray[0]; hiHz = S.freqArray[S.freqArray.length - 1]; }
  drawFreqRow(loHz, hiHz, col, p);

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

// -3dB measurement overlay
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

// ---- Limit line + pass/fail overlay (drawn under markers and OSD) ----
function renderLimits(powers: Float32Array | null) {
  const freq = S.freqArray;
  if (!powers || !freq) { updateLimitStatus(null); return; }
  const n = Math.min(powers.length, freq.length);
  const lim = buildLimitArray(freq, S.limits.points, n);
  if (!lim) { updateLimitStatus(null); return; }
  const p = PLOT_RECT;
  ctx.save();
  ctx.beginPath();
  ctx.rect(p.x, p.y, p.w, p.h);
  ctx.clip();
  // Bins above the limit: thicker red segments over the trace
  const runs = violationRuns(powers, lim, S.limits.tol);
  if (runs.length) {
    ctx.strokeStyle = 'rgba(255,64,64,0.95)';
    ctx.lineWidth = 2.5;
    for (const r of runs) {
      ctx.beginPath();
      for (let i = r.start; i <= r.end; i++) {
        const x = getX(i, n);
        const y = getY(powers[i]);
        if (i === r.start) ctx.moveTo(x, y); else ctx.lineTo(x, y);
      }
      ctx.stroke();
    }
  }
  // Dashed limit line itself
  ctx.setLineDash([6, 4]);
  ctx.lineWidth = 1.5;
  ctx.strokeStyle = '#ffb300';
  ctx.beginPath();
  for (let i = 0; i < n; i++) {
    const x = getX(i, n);
    const y = getY(lim[i]);
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.stroke();
  ctx.restore();
  updateLimitStatus(evaluateAgainst(powers, lim, freq, S.limits.tol));
}

export function renderAll() {
  const c = cur();
  // Waterfall container replaces the table slot in ALL modes (incl. RTA)
  const wfc = document.getElementById('waterfall-container');
  if (wfc) wfc.style.display = S.waterfallOn ? '' : 'none';
  if (c.viewMode === 'rta') {
    renderRta();
    renderWaterfallIfOn();
    updateLimitStatus(null);                        // limits are evaluated on the swept trace only
    const rp = getDisplayPowers();
    if (S.waterfallOn) {
      ['marker-table', 'peak-table', 'harmonic-table', 'pnm-table'].forEach((id) => {
        const el = document.getElementById(id);
        if (el) el.style.display = 'none';
      });
    } else if (S.peakListOn) {
      const mt2 = document.getElementById('marker-table');
      if (mt2) mt2.style.display = 'none';
      if (rp) { updatePeakTable(rp); renderPeakMarks(rp); }
    } else {
      const mt2 = document.getElementById('marker-table');
      if (mt2) mt2.style.display = '';
      const pt = document.getElementById('peak-table');
      if (pt) pt.style.display = 'none';   // pk list off -> no leftover peak table
      updateMarkerTable(rp);
    }
    return;
  }
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
  if (S.limits.on) renderLimits(powers);          // limit line + violations, under markers/OSD
  else updateLimitStatus(null);
  if (powers && S.freqArray) {
    if (c.viewMode !== 'harm' && c.viewMode !== 'pnm') {
      renderMarkersOnCanvas(powers);
      renderOSD(powers);
      render3dB(powers);
    }
    if (c.viewMode === 'harm') renderHarmonics(powers);
    if (c.measOn && c.measTabSel === 'amp') renderAmp(powers);
  }
  if (S.waterfallOn) {
    ['marker-table', 'peak-table', 'harmonic-table', 'pnm-table'].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.style.display = 'none';
    });
  } else {
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
  renderWaterfallIfOn();
}


// ---- RTA 实时频谱渲染 ----
function renderRta() {
  const col = canvasColors();
  ctx.clearRect(0, 0, W, H);
  const p = plotRect();
  // 背景 + 网格
  ctx.fillStyle = col.bg;
  ctx.fillRect(p.x, p.y, p.w, p.h);
  ctx.strokeStyle = col.grid;
  ctx.lineWidth = 1;
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
  // Y 轴标签
  ctx.fillStyle = col.axis; ctx.font = '11px monospace';
  ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  const labelX = p.x + p.w + 42;
  for (let i = 0; i <= S.totalDivs; i++) {
    const y = p.y + i * p.h / S.totalDivs;
    const v = cur().displayRef - i * cur().dbPerDiv;
    ctx.fillText(v.toFixed(0), labelX, y);
  }
  // Corner label "RTA" (kept; FFT size removed)
  ctx.fillStyle = col.axis; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
  ctx.fillText('RTA', p.x + 4, p.y + 4);
  const d = S.rtaData;
  if (!d || !d.freq || d.freq.length < 2) return;
  const n = d.freq.length;
  const lo = d.startHz, hi = d.stopHz;
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  // 2D probability density rendered as an offscreen layer (freq x amplitude matrix ->
  // ImageData with gamma-adjusted color), then drawImage-scaled onto the plot so the
  // hot region is continuous and smooth instead of sparse 1px dots. Row 0 = top of the
  // density matrix = displayRef (highest power), matching the plot Y direction.
  if (S.rtaDensity2d && S.rtaDensity2d.length >= n * S.RTA_AMP_BINS) {
    drawRtaDensityLayer(n, p);
  } else {
    ctx.fillStyle = col.bg;
    ctx.fillRect(p.x, p.y, p.w, p.h);
  }
  // Fluorescent traces with glow (previous style; density dots provide the lingering trail)
  S.traces.forEach((tr, ti) => {
    if (tr.mode === 'OFF') return;
    const disp = S.rtaDisplays[ti];
    if (!disp || disp.length < 2) return;
    const dn = Math.min(n, disp.length);   // guard against point-count mismatch
    const tcol = canvasColors().traces[ti] || col.rta;
    ctx.save();
    ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
    // glow
    ctx.globalAlpha = 0.22; ctx.lineWidth = 5; ctx.strokeStyle = tcol;
    ctx.beginPath();
    for (let i = 0; i < dn; i++) {
      const x = p.x + (d.freq[i] - lo) / (hi - lo) * p.w;
      const y = getY(disp[i]);
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    }
    ctx.stroke();
    // main line
    ctx.globalAlpha = 1; ctx.lineWidth = 1.5; ctx.strokeStyle = tcol;
    ctx.beginPath();
    for (let i = 0; i < dn; i++) {
      const x = p.x + (d.freq[i] - lo) / (hi - lo) * p.w;
      const y = getY(disp[i]);
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    }
    ctx.stroke();
    ctx.restore();
  });
  // 频率轴
  ctx.restore();   // close the outer clip (density + traces) so the bottom row outside the plot is visible
  // Bottom frequency row (same as SWP grid)
  drawFreqRow(lo, hi, col, p);
  // Markers on the active RTA trace (length-guarded)
  const actDisp = S.rtaDisplays[S.activeTraceIdx] || d.spec;
  renderMarkersOnCanvas(actDisp.length >= 2 ? actDisp : d.spec);
  renderOSD(actDisp.length >= 2 ? actDisp : d.spec);
}

// 瀑布: 渲染到容器内 canvas(容器替换 marker 表槽位, 布局稳定)
let lastSwpWfAt = 0;
function renderWaterfallIfOn() {
  if (!S.waterfallOn) return;
  const wf = document.getElementById('waterfall') as HTMLCanvasElement | null;
  if (!wf) return;
  // Canvas width = spectrum plot area width (CSS px), fixed (buttons don't squeeze it)
  const spec = document.getElementById('spectrum') as HTMLCanvasElement;
  const sRect = spec.getBoundingClientRect();
  const scale = sRect.width / S.W;
  const pr = plotRect();
  const wCss = pr.w * scale;                       // plot width in CSS px
  const leftCss = pr.x * scale;                    // plot left edge in CSS px
  const w = Math.max(60, Math.round(wCss));
  const h = Math.max(40, 135 - 2);
  if (wf.width !== w || wf.height !== h) { wf.width = w; wf.height = h; }
  // Pin the CSS box to the plot area: left AND right edge align with the spectrum X
  // axis (canvas is CSS-scaled, so the internal margins render at *scale on screen;
  // explicit width prevents the default canvas sizing from drifting)
  const ls = leftCss.toFixed(3) + 'px';
  if (wf.style.left !== ls) wf.style.left = ls;
  const ws = wCss.toFixed(3) + 'px';
  if (wf.style.width !== ws) wf.style.width = ws;
  // SWP mode: generate waterfall rows from the current trace (throttled ~10/s)
  if (!S.rtaMode) {
    const powers = getDisplayPowers();
    if (powers && !S.wfPaused) {
      const now = performance.now();
      if (now - lastSwpWfAt > 100) {
        lastSwpWfAt = now;
        pushSwpRow(powers, wf.width, 20);
      }
    }
  }
  renderWaterfall(wf, S.rtaMode ? 100 : 20);
}


let rtaDensLayer: HTMLCanvasElement | null = null;
let rtaDensCtx: CanvasRenderingContext2D | null = null;
let lastDensRebuild = 0;
// Build the density layer (throttled; cheap drawImage reuse between rebuilds)
function drawRtaDensityLayer(cols: number, p: { x: number; y: number; w: number; h: number }) {
  if (!rtaDensLayer) { rtaDensLayer = document.createElement('canvas'); rtaDensCtx = rtaDensLayer.getContext('2d'); }
  const rows = S.RTA_AMP_BINS;
  const now = performance.now();
  if (rtaDensLayer.width !== cols || rtaDensLayer.height !== rows) { rtaDensLayer.width = cols; rtaDensLayer.height = rows; }
  const lc = rtaDensCtx!;
  const dens = S.rtaDensity2d!;
  if (now - lastDensRebuild > 45) {   // ~22fps density refresh is plenty (fade is slow)
    lastDensRebuild = now;
    const lut = densityLutForRta();
    const img = lc.createImageData(cols, rows);
    const px = img.data;
    const MAXD = 40;   // density that saturates (matches the ws.ts +3/decay accumulation)
    for (let i = 0; i < cols; i++) {
      const base = i * rows;
      for (let b = 0; b < rows; b++) {
        const v = dens[base + b];
        const o = (i + b * cols) * 4;
        if (v <= 0.02) { px[o + 3] = 0; continue; }
        // gamma ~0.45: weak skirt/noise-floor hits stay clearly visible, dense saturate
        const g = Math.min(1, Math.pow(Math.min(1, v / MAXD), 0.45));
        const c = lut[Math.round(g * 255)];
        px[o] = c & 0xff; px[o + 1] = (c >> 8) & 0xff; px[o + 2] = (c >> 16) & 0xff;
        px[o + 3] = Math.min(255, 110 + Math.round(g * 145));
      }
    }
    lc.putImageData(img, 0, 0);
  }
  // smooth-bilinear scale onto the plot area (row 0 = top = displayRef)
  ctx.drawImage(rtaDensLayer, p.x, p.y, p.w, p.h);
}

// RTA density heat LUT (theme-aware, deep-blue -> cyan -> yellow -> red)
function densityLutForRta(): Uint32Array {
  const th = document.documentElement.dataset.theme;
  const stops = th === 'light'
    ? [[255, 255, 255], [200, 225, 255], [120, 170, 255], [60, 110, 220], [40, 130, 150], [180, 140, 0], [220, 70, 0]]
    : [[0, 8, 90], [0, 24, 150], [0, 70, 190], [0, 130, 200], [30, 200, 200], [90, 230, 90], [255, 220, 40], [255, 110, 20]];
  const lut = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    const x = i / 255 * (stops.length - 1);
    const j = Math.min(stops.length - 2, Math.floor(x));
    const t = x - j;
    const a = stops[j], b = stops[j + 1];
    const r = Math.round(a[0] + (b[0] - a[0]) * t);
    const g = Math.round(a[1] + (b[1] - a[1]) * t);
    const bl = Math.round(a[2] + (b[2] - a[2]) * t);
    lut[i] = (255 << 24) | (bl << 16) | (g << 8) | r;
  }
  return lut;
}
