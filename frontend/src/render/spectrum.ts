// Spectrum rendering main module: grid/traces/markers/OSD/3dB/peak marks
import * as S from '../core/store';
import { centerHz, spanHz } from '../ui/freqState';
import { getDisplayTop, getDisplayRange } from '../ui/displayRef';
import { ctx, onCanvasResize, pixelRatio, W, H } from '../core/store';
import { getX, getY, plotRect, plottableLevel } from './plot';
import { previewFreqWindow, previewLevelWindow, yAxisTransform } from '../ui/axisDrag';
import { canvasColors } from '../core/theme';
import { t } from '../core/i18n';
import { formatFreqHz, fmtAxis, fmtF } from '../core/fmt';
import { getDisplayPowers } from '../dsp/peaks';
import { getActiveAbsolutePowers } from '../dsp/absoluteTrace';
import { trigLevel, trigSource } from '../ui/triggerState';
import { smoothForDisplay } from '../dsp/smooth';
import { markerFreqHz } from '../core/markerCommon';
import { autoPeakThr, updatePeakTable, renderPeakMarks, peakListOn } from './peaklist';
import { updateMarkerTable } from './markerTable';
import { renderAmp } from '../meas/amplitude';
import { renderChannel, updateChanTable } from '../meas/channel';
import { renderWaterfall } from './waterfall';
import { setRenderer } from './redraw';
import { peakListVisible } from '../ui/measurePrefs';
import { getViewRenderer, registerViewRenderer } from './registry';
import { buildLimitArray, evaluateAgainst, violationRuns, type LimitEval } from '../dsp/limits';
import { pushStatus, renderStatusBlocks, resetStatusBlocks } from './statusStack';
import { rtaDensContrast, rtaDensWidth, rtaDensLayers, rtaDensSource, rtaHalflife, waterfallOn } from '../ui/waterfallState';
import { rasterDensity, densityLevelPlacement, densityLut, paintDensity, type DensityGrid } from '../dsp/rtaDensity';
import { visibleBinRange } from '../dsp/grid';
import { displayOffset, displayUnit, smoothBins } from '../ui/displayState';
import { fmtAxisLevel, fmtReadoutLevel } from '../core/level';
import { getCapture, getView, isZoomed } from '../ui/spectrumViewport';
import { getMarqueePreview } from '../ui/spectrumZoomGestures';
import { renderSpectrumZoomOverview } from './zoomOverview';
import { syncSpectrumZoomUi } from '../ui/spectrumZoomUi';
import { renderFilterHandles, renderFilterPassband } from '../ui/sdrFilterGesture';
import { renderSdrLocate } from '../ui/sdrLocate';
import { SDR_BADGE_FONT, sdrBadgeDragFrequency, sdrBadgeGeometry, sdrSpectrumWindow, validateSdrBadgeDrag } from '../ui/sdrBadgeGesture';

// Take mutable references from the store (snapshot at module level, re-read during render)
function cur() {
  // The axis labels follow a level gesture exactly like the trace does (ui/axisDrag.ts): both read
  // the same window, so the numbers and the pixels cannot disagree while the pointer is down.
  const lv = previewLevelWindow();
  return {
    centerHz: centerHz.get(), spanHz: spanHz.get(),
    dbPerDiv: lv ? lv.range / S.totalDivs : getDisplayRange() / S.totalDivs,
    displayRef: lv ? lv.ref : getDisplayTop(),
    displayOffset: displayOffset.get(), displayUnit: displayUnit.get(), viewMode: S.viewMode,
    measOn: S.measOn, measTabSel: S.measTabSel, traces: S.traces, markers: S.markers,
    activeMkrId: S.activeMkrId, freqArray: S.freqArray, m3dB: S.m3dB, harm: S.harm,
    ampRes: S.ampRes, peakListOn: peakListVisible.get(), smoothBins: smoothBins.get(),
  };
}

// Fixed plot rectangle: W/H/MARGIN move with the window, so this is recomputed per draw
// rather than cached - allocating one small object per frame is not the hot path (points are).
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

function drawMarqueeOverlay() {
  const m = getMarqueePreview();
  if (!m) return;
  const view = isZoomed() ? getView() : getCapture();
  if (!view || !(view.hi > view.lo)) return;
  const p = plotRect();
  const x1 = p.x + (m.lo - view.lo) / (view.hi - view.lo) * p.w;
  const x2 = p.x + (m.hi - view.lo) / (view.hi - view.lo) * p.w;
  const col = canvasColors();
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  ctx.fillStyle = 'rgba(0,229,255,0.08)';
  ctx.fillRect(x1, p.y, Math.max(1, x2 - x1), p.h);
  ctx.strokeStyle = col.text;
  ctx.lineWidth = 1.2;
  ctx.setLineDash([5, 4]);
  ctx.strokeRect(x1, p.y, Math.max(1, x2 - x1), p.h);
  ctx.setLineDash([]);
  ctx.fillStyle = col.text;
  ctx.fillRect(x1 - 1, p.y, 2, p.h);
  ctx.fillRect(x2 - 1, p.y, 2, p.h);
  ctx.restore();
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

  drawYAxisLabels(p, col, c.displayRef, c.dbPerDiv);

  let loHz = c.centerHz - c.spanHz / 2, hiHz = c.centerHz + c.spanHz / 2;
  if (S.freqArray && S.freqArray!.length > 1) { loHz = S.freqArray![0]; hiHz = S.freqArray![S.freqArray!.length - 1]; }
  // The frequency row states the window the gesture is asking for, not the one the frame on
  // screen was measured in, so the numbers track the drag exactly.
  const pw = previewFreqWindow();
  if (pw) { loHz = pw.lo; hiHz = pw.hi; }
  else if (isZoomed()) {
    // Display-only zoom: the row states the VIEW window (what the pixels show), while the
    // acquisition values above stay untouched (design §3).
    const v = getView();
    if (v) { loHz = v.lo; hiHz = v.hi; }
  }
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
  const n = t.powers.length;
  ctx.strokeStyle = col.traces[t.id - 1] || col.traces[0];
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  let pen = false;
  const view = previewFreqWindow() ?? (isZoomed() ? getView() : getCapture());
  const freq = S.freqArray;
  const [first, end] = view && freq ? visibleBinRange(freq, view.lo, view.hi) : [0, n];
  // Adaptive smoothing uses the full frame, as do marker/readout levels. A slice-local
  // percentile changes the apparent amplitude when the viewport moves.
  const data = smoothBins.get() > 1 ? smoothForDisplay(t.powers, t.mode) : t.powers;
  const nativeAxis = freq && freq.length === n ? freq : null;
  const axisFirst = nativeAxis ? nativeAxis[0] : 0;
  const axisLast = nativeAxis ? nativeAxis[n - 1] : n - 1;
  const xFirst = getX(0, n);
  const xScale = (getX(n - 1, n) - xFirst) / (axisLast - axisFirst);
  const after = Math.min(n, end);
  let i = first;
  while (i < after) {
    if (!plottableLevel(data[i])) { pen = false; i++; continue; }
    const start = i;
    const column = Math.floor((xFirst + ((nativeAxis ? nativeAxis[i] : i) - axisFirst) * xScale - p.x) * pixelRatio);
    let min = i, max = i;
    i++;
    while (i < after && plottableLevel(data[i])
        && Math.floor((xFirst + ((nativeAxis ? nativeAxis[i] : i) - axisFirst) * xScale - p.x) * pixelRatio) === column) {
      if (data[i] < data[min]) min = i;
      if (data[i] > data[max]) max = i;
      i++;
    }
    const last = i - 1;
    let previous = -1;
    for (let vertex = 0; vertex < 4; vertex++) {
      const index = vertex === 0 ? start : vertex === 3 ? last
        : vertex === 1 ? Math.min(min, max) : Math.max(min, max);
      if (index === previous) continue;
      previous = index;
      const x = xFirst + ((nativeAxis ? nativeAxis[index] : index) - axisFirst) * xScale;
      const y = getY(data[index]);
      if (!pen) { ctx.moveTo(x, y); pen = true; } else ctx.lineTo(x, y);
    }
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

// ---- Top-left plot text stack -------------------------------------------------------------
//
// One owner for that corner. Line 0 belongs to the corner slot: the RTA or SDR mode badge in the
// real-time view, the 3 dB BW label in the swept view, and the H labels of the harmonic view.
// The cursor readout shares that line when the mode badge has it, so the operator reads the mode
// and the hovered point as one row. The readout takes the next line when a wide label owns line 0.
// The marker readouts follow it. One place computes the line numbers. Thus a hover cannot cover
// the mode badge, and a hover never moves the badge.
const TL_LINE = 14;

/** The text of the mode badge in the real-time view. renderRta draws the badge on line 0. */
function modeBadgeText(): string {
  return S.sdrMode ? 'SDR' : 'RTA';
}

/** The right edge of the RTA/SDR badge. The cursor readout starts after it. */
function modeBadgeRight(): number {
  ctx.font = '11px monospace';
  return plotRect().x + 4 + ctx.measureText(modeBadgeText()).width + 4;
}

/**
 * Draw the RTA/SDR corner badge. The badge holds the mode name and a plate. The border colour is
 * the text colour (col.axis), so the badge and the readout on the same line read as one row.
 */
function drawModeBadge() {
  const p = plotRect();
  const col = canvasColors();
  const x = p.x + 4;
  const y = tlY(0);
  const w = modeBadgeRight() - x;
  ctx.font = '11px monospace';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'bottom';
  ctx.fillStyle = col.labelBg;
  ctx.fillRect(x, y, w, 12);
  ctx.strokeStyle = col.axis;
  ctx.lineWidth = 1;
  ctx.strokeRect(x, y, w, 12);
  ctx.fillStyle = col.axis;
  ctx.fillText(modeBadgeText(), x + 2, y + 12);
}

/**
 * The line of the top-left stack for each block in this frame, and whether the readout shares the
 * line of the mode badge.
 */
export function topLeftLines(): { readout: number; osd: number; shareMode: boolean } {
  const c = cur();
  const modeBadge = c.viewMode === 'rta';
  // A view that owns line 0 with its own labels (harmonic, phase noise), or the 3 dB BW label.
  // The 3 dB label is on the swept path only: a stale reading must not move the readout in the
  // RTA view.
  const lineZeroLabel = (!modeBadge && !!getViewRenderer(c.viewMode))
    || (!getViewRenderer(c.viewMode) && !!c.m3dB);
  const readout = modeBadge ? 0 : (lineZeroLabel ? 1 : 0);
  return { readout, osd: readout + 1, shareMode: modeBadge };
}

/** The y position of a top-left stack line. The text baseline is 'top'. */
function tlY(line: number): number {
  return plotRect().y + 4 + line * TL_LINE;
}

/**
 * Cursor readout: the frequency and the level of the bin under the pointer. In the real-time view
 * the readout shares the line of the mode badge and its colour (col.axis). The readout keeps the
 * plate behind the text and no border, so a row of readouts does not compete with the badge. The
 * number format is the format of the marker readouts: fmtReadoutLevel applies the display unit
 * and the external offset. The function draws nothing while the pointer is outside the plot.
 */
function renderCursorReadout() {
  const idx = S.plotCursorIdx;
  const powers = getDisplayPowers();
  const usable = idx !== null && !!powers && powers.length > 1;
  const i = usable ? Math.max(0, Math.min(powers!.length - 1, idx!)) : 0;
  const text = usable
    ? `${formatFreqHz(markerFreqHz(i))}  ${fmtReadoutLevel(powers![i])}`
    : '';
  // Diagnostic/e2e hook (same pattern as dataset.notice). The readout is canvas text, so the value
  // must be readable from outside the canvas. Written only on change.
  if (ctx.canvas.dataset.cursor !== text) ctx.canvas.dataset.cursor = text;
  if (!usable) return;
  const lines = topLeftLines();
  const col = canvasColors();
  ctx.font = '11px monospace';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'bottom';
  const w = ctx.measureText(text).width + 4;
  const x = lines.shareMode ? modeBadgeRight() + 6 : plotRect().x + 4;
  const y = tlY(lines.readout);
  ctx.fillStyle = col.labelBg;
  ctx.fillRect(x, y, w, 12);
  ctx.fillStyle = col.axis;
  ctx.fillText(text, x + 2, y + 12);
}

function renderOSD(powers: Float32Array) {
  const c = cur();
  const col = canvasColors();
  const active = c.markers.filter(m => m.enabled && m.mode !== 'OFF');
  if (!active.length) return;
  const p = plotRect();
  ctx.font = '11px monospace';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'top';
  const top = tlY(topLeftLines().osd);
  let line = 0;
  active.forEach(m => {
    const cy = top + line * TL_LINE;
    const idx = Math.min(m.idx, powers.length - 1);
    const f = markerFreqHz(idx), a = powers[idx];
    let txt: string;
    if (m.mode === 'NORMAL') {
      // Match marker-table units and external offset; relative values remain raw dB.
      txt = `M${m.id} ${formatFreqHz(f)}  ${fmtReadoutLevel(a)}`;
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
let lastLimitEval: LimitEval | null = null;

/** Limit verdict as a status block; the stack owns the canvas status area. */
function limitStatusBlock(): { lines: string[]; accent?: string } | null {
  if (!S.limits.on) return null;
  if (!S.rtaMode && S.traces[S.activeTraceIdx]?.isNormalized) {
    return { lines: [t('absolute_trace_unavailable')] };
  }
  const ev = lastLimitEval;
  if (!ev || !ev.checked) return { lines: [t('limit_canvas_na')] };
  if (ev.pass) return { lines: [t('limit_canvas_pass')], accent: '#00c853' };
  const w = ev.worst!;
  return {
    lines: [
      `!${t('limit_canvas_fail', { n: ev.violations })}`,
      `!${t('limit_canvas_worst', { db: w.margin.toFixed(1), f: fmtF(w.freqHz) })}`,
    ],
    accent: '#ff5252',
  };
}

function renderLimits() {
  const powers = getActiveAbsolutePowers();
  const freq = S.freqArray;
  if (!powers || !freq) {
    lastLimitEval = null;
    const canvas = document.getElementById('spectrum');
    if (canvas) delete canvas.dataset.limitsDbg;
    return;
  }
  const n = Math.min(powers.length, freq.length);
  const lim = buildLimitArray(freq, S.limits.points, n);
  if (!lim) { lastLimitEval = null; return; }
  const p = plotRect();
  ctx.save();
  ctx.beginPath();
  ctx.rect(p.x, p.y, p.w, p.h);
  ctx.clip();
  // Draw the limit line and PASS/FAIL status; retain overlay geometry for diagnostics.
  const runs = violationRuns(powers, lim, S.limits.tol);
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
  lastLimitEval = evaluateAgainst(powers, lim, freq, S.limits.tol);
  {
    // Debug/verification aid: the geometry the overlay actually used.
    const cv = document.getElementById('spectrum');
    if (cv) {
      let peakIdx = 0, peakVal = -1e9;
      for (let i = 0; i < n; i++) {
        const v = powers[i];
        if (Number.isFinite(v) && v > peakVal) { peakVal = v; peakIdx = i; }
      }
      cv.dataset.limitsDbg = JSON.stringify({
        n, tol: S.limits.tol,
        runs: runs.map((r) => [r.start, r.end]),
        above: runs.reduce((a, r) => a + (r.end - r.start + 1), 0),
        peakIdx, peakDbm: Math.round(peakVal * 10) / 10,
        limAtPeak: Math.round(lim[peakIdx] * 10) / 10,
        margin: Math.round((peakVal - lim[peakIdx]) * 10) / 10,
      });
    }
  }
}

/** Drawing primitives shared with registered views. */
function viewContext() {
  return { renderGrid, renderTraceLine, getDisplayPowers };
}

/** RTA/SDR view: the real-time trace, trigger overlay, limits and the table swaps. */
function renderRtaView() {

    renderRta();
    renderTriggerLevel();                           // outside renderRta: still drawn when the
    renderTriggerOverlay();                         // canvas is empty while waiting
    if (S.limits.on) { renderLimits(); pushStatus(limitStatusBlock()); }
    if (S.badData) pushStatus({ lines: [`!${t('bad_data')}`], accent: '#ff7043' });
    renderWaterfallIfOn();
    const rp = getDisplayPowers();
    // The RTA/SDR path needs the same auto peak threshold as the swept path, otherwise the
    // marker peak-search uses the swept (or HTML default) threshold and jumps onto the noise
    // floor instead of the signal.
    if (rp) autoPeakThr(rp);
    if (waterfallOn.get()) {
      ['marker-table', 'peak-table', 'harmonic-table', 'pnm-table'].forEach((id) => {
        const el = document.getElementById(id);
        if (el) el.style.display = 'none';
      });
    } else if (peakListVisible.get()) {
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
    drawMarqueeOverlay();
    renderSpectrumZoomOverview();
    syncSpectrumZoomUi();
    return;
  }

export function renderAll() {
  validateSdrBadgeDrag();
  const c = cur();
  resetStatusBlocks();                             // status area is rebuilt every pass
  // Waterfall container replaces the table slot in ALL modes (incl. RTA)
  const wfc = document.getElementById('waterfall-container');
  if (wfc) wfc.style.display = waterfallOn.get() ? '' : 'none';

  // Use the swept path when no renderer is registered for the active mode.
  const view = getViewRenderer(c.viewMode);
  if (view) {
    view.render(viewContext());
    renderStatusBlocks();
    renderCursorReadout();          // top-left, drawn over every view's own labels
    drawMarqueeOverlay();
    renderSpectrumZoomOverview();
    syncSpectrumZoomUi();
    return;
  }
  renderGrid();
  c.traces.forEach(t => renderTraceLine(t));
  const powers = getDisplayPowers();
  if (S.swpArmed || S.swpHold) {           // SWP software trigger is visual too
    renderTriggerLevel();
    renderTriggerOverlay();
  }
  if (S.limits.on) { renderLimits(); pushStatus(limitStatusBlock()); }
  if (powers && S.freqArray) {
    // Markers/OSD/3 dB are drawn only on the swept path; the measurement views own their
    // own overlays (see render/registry.ts). The amplitude/channel measurements are also
    // overlays on the swept trace, so they stay here.
    renderMarkersOnCanvas(powers);
    renderOSD(powers);
    render3dB(powers);
    if (c.measOn && c.measTabSel === 'amp') renderAmp(powers);
    if (c.measOn && c.measTabSel === 'chan') renderChannel(powers);
  }
  renderCursorReadout();
  drawMarqueeOverlay();
  if (waterfallOn.get()) {
    ['marker-table', 'peak-table', 'harmonic-table', 'pnm-table'].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.style.display = 'none';
    });
  } else if (powers) {
    autoPeakThr(powers);
    if (peakListOn() && c.viewMode === 'std') {
      const mt = document.getElementById('marker-table');
      if (mt) mt.style.display = 'none';
      updatePeakTable(powers);
      renderPeakMarks(powers);
    } else if (c.measOn && c.measTabSel === 'chan') {
      const mt = document.getElementById('marker-table');
      if (mt) mt.style.display = 'none';
      const pt = document.getElementById('peak-table');
      if (pt) pt.style.display = 'none';
      updateChanTable();
    } else {
      const mt = document.getElementById('marker-table');
      if (mt) mt.style.display = '';
      const pt = document.getElementById('peak-table');
      if (pt) pt.style.display = 'none';
      updateMarkerTable(powers);
    }
  }
  renderStatusBlocks();
  renderWaterfallIfOn();
  renderSpectrumZoomOverview();
  syncSpectrumZoomUi();
}

// Repaint callers depend on redraw.ts; this module owns the renderer.
setRenderer(renderAll);
// The real-time view is part of the hub itself, so it registers locally.
registerViewRenderer({ mode: 'rta', render: () => renderRtaView() });


/**
 * Y-axis tick labels (right of the graticule), in the display domain.
 *
 * The trace is displaced by the external offset in getY(); the axis labels convert the unit
 * only (fmtAxisLevel), so the number on a graticule line equals the readout of the point
 * drawn on it - the offset is applied exactly once.
 */
function drawYAxisLabels(p: { x: number; y: number; w: number; h: number },
                         col: { axis: string }, displayRef: number, dbPerDiv: number) {
  ctx.fillStyle = col.axis; ctx.font = '11px monospace';
  ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  const labelX = p.x + p.w + 42;
  const labels: string[] = [];
  for (let i = 0; i <= S.totalDivs; i++) {
    const y = p.y + i * p.h / S.totalDivs;
    const text = fmtAxisLevel(displayRef - i * dbPerDiv);
    labels.push(text);
    ctx.fillText(text, labelX, y);
  }
  // Written only on change (this runs on every pass; they change with ref/dB-per-div/offset/unit).
  const text = labels.join(',');
  if (ctx.canvas.dataset.yLabels !== text) ctx.canvas.dataset.yLabels = text;
  // Diagnostic/e2e hook: which display domain the axis is in. It decides what the level-axis
  // gesture means (the device reference in dBm, the level offset in the relative display), and
  // canvas text cannot tell the two apart from outside.
  const unit = displayUnit.get();
  if (ctx.canvas.dataset.levelUnit !== unit) ctx.canvas.dataset.levelUnit = unit;
}

// Persistent trigger status chip (top-right) plus the warning lines under it. It is drawn
// even when the packet stream is empty (waiting), so the canvas always says which mode it
// is in and never looks like a stale or broken picture.
function renderTriggerOverlay() {
  const lines = S.trigOverlay;
  pushStatus({
    lines,
    accent: S.trigHit ? '#00c853' : (S.trigWaiting ? '#ff7043' : '#00cc00'),
  });
}

// Trigger threshold line (RTA only): shows where a level trigger will fire.
function renderTriggerLevel() {
  if (trigSource.get() !== 'level' && !S.swpArmed && !S.swpHold) return;
  const p = plotRect();
  const raw = getY(trigLevel.get());
  if (!Number.isFinite(raw)) return;
  // Changing Ref (manually or through Auto Ref) re-scales the display; keep the threshold
  // visible by pinning it to the nearest edge instead of dropping the line entirely.
  const offscreen = raw < p.y ? -1 : raw > p.y + p.h ? 1 : 0;
  const y = Math.min(Math.max(raw, p.y + 1), p.y + p.h - 1);
  ctx.save();
  ctx.setLineDash([6, 4]);
  ctx.lineWidth = 1.5;
  ctx.strokeStyle = '#ff7043';
  ctx.beginPath();
  ctx.moveTo(p.x, y);
  ctx.lineTo(p.x + p.w, y);
  ctx.stroke();
  ctx.setLineDash([]);
  ctx.fillStyle = '#ff7043';
  ctx.font = 'bold 10px monospace';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'bottom';
  ctx.fillText(`${offscreen ? (offscreen < 0 ? '\u25b2 ' : '\u25bc ') : ''}TRG ${trigLevel.get().toFixed(1)} dBm`,
    p.x + 4, offscreen < 0 ? y + 11 : y - 2);
  ctx.restore();
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
  drawYAxisLabels(p, col, cur().displayRef, cur().dbPerDiv);
  // Corner badge "RTA"/"SDR". The cursor readout continues on this line (topLeftLines()).
  drawModeBadge();
  const d = S.rtaData;
  if (!d || !d.freq || d.freq.length < 2) return;
  const n = d.freq.length;
  const window = sdrSpectrumWindow();
  if (!window) return;
  const { lo, hi } = window;
  ctx.save();
  ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  const activeTrace = S.traces[S.activeTraceIdx];
  const densityOn = rtaHalflife.get() > 0 && rtaDensLayers.get() !== 'trace'
    && (rtaDensSource.get() === 'live' || activeTrace.mode !== 'OFF');
  const density = S.rtaDensity2d;
  const densityVisible = densityOn && !!density && (!density.freq || density.freq === d.freq);
  // Native history is rasterised only for this visible Hz window at screen resolution.
  if (densityVisible) drawRtaDensityLayer(density!, d.freq, lo, hi, p);
  // Without density, retain the graticule and background. clearRect runs at frame start.
  // Diagnostic/e2e hook (same pattern as `dataset.yLabels`): the pixel scan cannot tell a dark
  // density layer from the plain background, so the contract is read from here.
  const densityState = densityVisible ? 'on' : 'off';
  if (ctx.canvas.dataset.density !== densityState) ctx.canvas.dataset.density = densityState;
  // A drawn density layer carries the lingering trail, so the active trace drops its wide
  // glow and stays a thin line: overlapping layers would only hide the density below.
  const thinTraces = densityVisible;
  // Until the first frame of a new history arrives, retain the trace instead of showing an
  // empty plot. The live source also remains drawable when its trace is OFF.
  const hideTraces = rtaDensLayers.get() === 'density' && densityVisible;
  const [firstBin, afterBin] = visibleBinRange(d.freq, lo, hi);
  const xScale = p.w / (hi - lo);
  const levelOffset = displayOffset.get();
  const levelRef = getDisplayTop();
  const yScale = p.h / getDisplayRange();
  const levelTransform = yAxisTransform();
  S.traces.forEach((_tr, ti) => {
    if (hideTraces) return;
    const disp = S.rtaDisplays[ti];
    if (!disp || disp.length < 2) return;
    const dn = Math.min(n, disp.length);   // guard against point-count mismatch
    const tcol = col.traces[ti] || col.rta;
    ctx.save();
    ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
    ctx.beginPath();
    let pen = false;
    const end = Math.min(dn, afterBin);
    // Keep native samples in storage; the canvas only needs each device column's
    // first/min/max/last original vertices. Split runs at every missing bin.
    let i = firstBin;
    while (i < end) {
      if (!plottableLevel(disp[i])) { pen = false; i++; continue; }
      const first = i;
      const column = Math.floor((d.freq[i] - lo) * xScale * pixelRatio);
      let min = i, max = i;
      i++;
      while (i < end && plottableLevel(disp[i])
          && Math.floor((d.freq[i] - lo) * xScale * pixelRatio) === column) {
        if (disp[i] < disp[min]) min = i;
        if (disp[i] > disp[max]) max = i;
        i++;
      }
      const last = i - 1;
      let previous = -1;
      for (let vertex = 0; vertex < 4; vertex++) {
        const index = vertex === 0 ? first : vertex === 3 ? last
          : vertex === 1 ? Math.min(min, max) : Math.max(min, max);
        if (index === previous) continue;
        previous = index;
        const x = p.x + (d.freq[index] - lo) * xScale;
        let y = p.y + (levelRef - (disp[index] + levelOffset)) * yScale;
        if (levelTransform) y = p.y + (y - p.y) * levelTransform.sy + levelTransform.dyFrac * p.h;
        if (!pen) { ctx.moveTo(x, y); pen = true; } else ctx.lineTo(x, y);
      }
    }
    // SDR uses a single fine stroke; persistence already supplies its measured trail.
    if (!thinTraces && !S.sdrMode) {
      ctx.globalAlpha = 0.22; ctx.lineWidth = 5; ctx.strokeStyle = tcol;
      ctx.stroke();
    }
    // main line
    ctx.globalAlpha = 1; ctx.lineWidth = S.sdrMode ? 1 : 1.5; ctx.strokeStyle = tcol;
    ctx.stroke();
    ctx.restore();
  });
  // Preview the dial and passband together while the badge owns the pointer.
  renderFilterPassband(lo, hi, sdrBadgeDragFrequency());
  renderSdrLocate(lo, hi);
  ctx.restore();   // close the outer clip (density + traces) so the bottom row outside the plot is visible
  renderFilterHandles();
  // Bottom frequency row (same as SWP grid)
  drawFreqRow(lo, hi, col, p);
  const badge = sdrBadgeGeometry();
  if (badge) {
    ctx.save();
    ctx.font = SDR_BADGE_FONT;
    ctx.fillStyle = col.rta;
    ctx.fillRect(badge.left, badge.top, badge.width, badge.height);
    ctx.fillStyle = col.bg;
    ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
    ctx.fillText(badge.label, badge.left + badge.width / 2, badge.top + badge.height / 2, badge.width - 8);
    ctx.restore();
  }
  // Markers on the active RTA trace (length-guarded)
  const actDisp = S.rtaDisplays[S.activeTraceIdx] || d.spec;
  renderMarkersOnCanvas(actDisp.length >= 2 ? actDisp : d.spec);
  renderOSD(actDisp.length >= 2 ? actDisp : d.spec);
}

// 瀑布: 渲染到容器内 canvas(容器替换 marker 表槽位, 布局稳定)
// The waterfall canvas is sized here, never inside the frame loop: reading clientWidth/
// clientHeight per frame forces a layout per frame (render/dsp/meas are covered by
// hotPathLayout.test.ts). It is re-derived when the plot box changes - core/store.ts pushes
// that through onCanvasResize below - and when the waterfall is switched on.
let wfSized = false;
onCanvasResize(() => { wfSized = false; });

function sizeWaterfall(wf: HTMLCanvasElement): void {
  // Drawing units are CSS px of the spectrum content box, so the waterfall's CSS box is the
  // plot rectangle as-is: no scale factor, no conversion.
  const pr = plotRect();
  const left = pr.x.toFixed(3) + 'px';
  if (wf.style.left !== left) wf.style.left = left;
  const width = pr.w.toFixed(3) + 'px';
  if (wf.style.width !== width) wf.style.width = width;
  // Size the backing store from the actual content box and spectrum pixel ratio.
  // Browser rounding requires measuring the box. layout-read-ok: resize/zoom/enable only.
  const backW = Math.max(60, Math.round(wf.clientWidth * pixelRatio));    // layout-read-ok: resize path only
  const backH = Math.max(40, Math.round(wf.clientHeight * pixelRatio));   // layout-read-ok: resize path only
  if (wf.width !== backW) wf.width = backW;
  if (wf.height !== backH) wf.height = backH;
  wfSized = true;
}

function renderWaterfallIfOn() {
  if (!waterfallOn.get()) {
    wfSized = false;
    return;
  }
  const wf = document.getElementById('waterfall') as HTMLCanvasElement | null;
  if (!wf) return;
  if (!wfSized) sizeWaterfall(wf);
  renderWaterfall(wf, 100);
}


let rtaDensLayer: HTMLCanvasElement | null = null;
let rtaDensCtx: CanvasRenderingContext2D | null = null;
let rtaDensImage: ImageData | null = null;
let rtaDensRaster = new Float32Array(0);
let lastDensGrid: DensityGrid | null = null;
let lastDensFreq: Float64Array | null = null;
let lastDensRevision = -1;
let lastDensLo = NaN, lastDensHi = NaN;
let lastDensTheme: string | undefined;
let lastDensContrast = NaN;
let lastDensWidth = NaN, lastDensCentre = NaN, lastDensPitch = NaN;
let rtaDensLut: Uint32Array | null = null;
let lastDensSoftFootprint = false;


function drawRtaDensityLayer(
  grid: DensityGrid, freq: Float64Array, lo: number, hi: number,
  p: { x: number; y: number; w: number; h: number },
) {
  if (!rtaDensLayer) {
    rtaDensLayer = document.createElement('canvas');
    rtaDensCtx = rtaDensLayer.getContext('2d');
  }
  const cols = Math.max(1, Math.ceil(p.w * pixelRatio));
  const rows = Math.max(1, Math.ceil(p.h * pixelRatio));
  const resized = rtaDensLayer.width !== cols || rtaDensLayer.height !== rows;
  if (resized) { rtaDensLayer.width = cols; rtaDensLayer.height = rows; }
  const lc = rtaDensCtx!;
  const theme = document.documentElement.dataset.theme;
  const themeChanged = lastDensTheme !== theme || !rtaDensLut;
  if (themeChanged) { rtaDensLut = densityLut(theme === 'light'); lastDensTheme = theme; }
  const contrast = rtaDensContrast.get();
  let softFootprint = false;
  if (S.sdrMode && hi > lo) {
    const [first, end] = visibleBinRange(freq, lo, hi);
    // Only soften detail views where native frequency spacing exceeds two CSS pixels.
    softFootprint = end - first > 1
      && (freq[end - 1] - freq[first]) * p.w > 2 * (hi - lo) * (end - first - 1);
  }
  // Reproject frozen history on every offset/reference/scale/gesture change. The footprint
  // remains CSS pixels even when the native amplitude grid stretches or the DPR changes.
  const level = densityLevelPlacement(grid, getDisplayTop(), getDisplayRange(), displayOffset.get());
  const yt = yAxisTransform();
  const sy = yt ? yt.sy : 1;
  const pitch = level.height / grid.bins * sy * rows;
  const centre = (level.top * sy + (yt ? yt.dyFrac : 0)) * rows + pitch / 2;
  const width = rtaDensWidth.get() * rows / p.h;
  const rasterChanged = resized || lastDensGrid !== grid || lastDensRevision !== grid.revision
    || lastDensFreq !== freq || lastDensLo !== lo || lastDensHi !== hi
    || lastDensSoftFootprint !== softFootprint;
  if (rasterChanged) {
    if (rtaDensRaster.length !== cols * grid.bins) rtaDensRaster = new Float32Array(cols * grid.bins);
    rasterDensity(grid, freq, lo, hi, cols, rtaDensRaster);
  }
  if (rasterChanged || themeChanged || lastDensContrast !== contrast
      || lastDensWidth !== width || lastDensCentre !== centre || lastDensPitch !== pitch) {
    if (!rtaDensImage || rtaDensImage.width !== cols || rtaDensImage.height !== rows) {
      rtaDensImage = lc.createImageData(cols, rows);
    }
    paintDensity(rtaDensRaster, cols, grid.bins, rtaDensImage.data, rows,
      centre, pitch, width, contrast, rtaDensLut!, softFootprint);
    lc.putImageData(rtaDensImage, 0, 0);
    lastDensGrid = grid; lastDensRevision = grid.revision;
    lastDensFreq = freq;
    lastDensLo = lo; lastDensHi = hi; lastDensContrast = contrast;
    lastDensWidth = width; lastDensCentre = centre; lastDensPitch = pitch;
    lastDensSoftFootprint = softFootprint;
  }
  ctx.save();
  ctx.imageSmoothingEnabled = softFootprint;
  ctx.drawImage(rtaDensLayer, p.x, p.y, p.w, p.h);
  ctx.restore();
}
