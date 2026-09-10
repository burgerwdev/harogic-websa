// Trace UI ops: tab switching / mode setting
import * as S from '../core/store';
import { updateInfoBar } from '../render/infobar';
import { updateNormalizeStatusUI } from '../dsp/normalize';
import { applyTraceMode, resetTraceAccum } from '../dsp/traces';
import { setAverageCount } from '../dsp/accumulator';
import { renderAll } from '../render/spectrum';

// Freeze (View) toggle button state — reflects the active trace's mode
export function syncFreezeBtn() {
  const t = S.traces[S.activeTraceIdx];
  const btn = document.getElementById('btn-view-freeze');
  if (!btn) return;
  btn.classList.toggle('active', t.mode === 'VIEW');
}

export function toggleFreeze() {
  const t = S.traces[S.activeTraceIdx];
  if (t.mode === 'VIEW') {
    setTraceMode(t.prevMode && t.prevMode !== 'VIEW' ? t.prevMode : 'CLEAR_WRITE');
  } else {
    t.prevMode = t.mode;
    setTraceMode('VIEW');
  }
  renderAll();
}

export function switchTraceTab(idx: number) {
  S.setActiveTraceIdx(idx);
  document.querySelectorAll('.trace-btn').forEach(b => b.classList.remove('active'));
  const btns = document.querySelectorAll('.trace-btn');
  if (btns[idx]) btns[idx].classList.add('active');
  const t = S.traces[idx];
  const sel = document.getElementById('select-trace-mode') as HTMLSelectElement;
  if (sel) sel.value = t.mode === 'VIEW' ? (t.prevMode || 'CLEAR_WRITE') : t.mode;
  syncFreezeBtn();
  const anyNorm = S.traces.some(x => x.isNormalized && x.reference);
  S.setDisplayUnit((t.reference && t.isNormalized) ? 'dB' : (anyNorm ? 'dB' : 'dBm'));
  S.setDisplayRef(S.displayUnit === 'dB' ? 0.0 : S.refLevel);
  updateNormalizeStatusUI();
  updateInfoBar();
}

export function clearRtaTrace() {
  // Clear the active trace + the probability-density background so the spectrum restarts
  // fresh from the live signal. The waterfall has its own Clear (wf-reset) and is left
  // untouched here.
  const idx = S.activeTraceIdx;
  const arr = S.rtaDisplays.slice();
  arr[idx] = null;
  S.rtaAvgN[idx] = 0;
  S.rtaAvgSum[idx] = null;
  S.rtaDone[idx] = false;
  S.setRtaDisplays(arr);
  if (S.rtaDensity2d) S.rtaDensity2d.fill(0);
  renderAll();
}

export function setTraceMode(mode: string) {
  const idx = S.activeTraceIdx;
  applyTraceMode(S.traces[idx], mode);
  resetRtaAverage(idx);
  syncFreezeBtn();
  syncAvgUI();
}

/**
 * RTA stores its own accumulator arrays, so SWP-side resets are not enough: a stale
 * avgSum/avgCount would make a fresh average start near full scale and decay slowly
 * (reported as "the trace descends from the top").
 */
function resetRtaAverage(idx: number): void {
  S.rtaAvgSum[idx] = null;
  S.rtaAvgN[idx] = 0;
  S.rtaDone[idx] = false;
}

/** Average count UI: select value + "count/target" status for the active trace. */
export function syncAvgUI(): void {
  const idx = S.activeTraceIdx;
  const t = S.traces[idx];
  const row = document.getElementById('trace-avg-row');
  if (row) row.style.display = t.mode === 'AVERAGE' ? '' : 'none';
  const sel = document.getElementById('select-trace-avg') as HTMLSelectElement | null;
  if (sel && document.activeElement !== sel) sel.value = String(t.avgTarget ?? 16);
  const st = document.getElementById('trace-avg-status');
  if (!st) return;
  if (t.mode !== 'AVERAGE') { st.textContent = ''; return; }
  // RTA keeps its own accumulator, so the frame count lives in rtaAvgN (not traces[i].avgCount).
  const count = S.rtaMode ? S.rtaAvgN[idx] : t.avgCount;
  st.textContent = t.avgTarget ? `${t.avgTarget}` : `∞ (${count})`;
}

export function setTraceAverage(count: number): void {
  const idx = S.activeTraceIdx;
  setAverageCount(S.traces[idx], count);
  resetRtaAverage(idx);
  syncAvgUI();
  renderAll();
}

/** Export the active trace as CSV (metadata header + freq/power pairs). */
export function exportActiveTraceCsv(): void {
  const t = S.traces[S.activeTraceIdx];
  const powers = t?.powers;
  const freq = S.freqArray;
  if (!t || !powers || !freq) return;
  const n = Math.min(powers.length, freq.length);
  const head = [
    `# trace=T${t.id}`, `mode=${t.mode}`,
    `center_hz=${S.centerHz}`, `span_hz=${S.spanHz}`,
    `rbw_hz=${S.currentRBW}`, `vbw_hz=${S.currentVBW}`,
    `display_unit=${S.displayUnit}`, `normalized=${t.isNormalized}`,
    `smooth_bins=${S.smoothBins}`, `time=${new Date().toISOString()}`,
    'freq_hz,power',
  ];
  const rows: string[] = [];
  for (let i = 0; i < n; i++) {
    const v = powers[i];
    rows.push(`${Number(freq[i]).toFixed(3)},${isFinite(v) ? v.toFixed(3) : ''}`);
  }
  const blob = new Blob([head.concat(rows).join('\n')], { type: 'text/csv;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `websa_T${t.id}_${new Date().toISOString().replace(/[:.]/g, '-')}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}
