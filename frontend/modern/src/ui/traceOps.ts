// Trace UI ops: tab switching / mode setting
import * as S from '../core/store';
import { updateInfoBar } from '../render/infobar';
import { updateNormalizeStatusUI } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';
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
  const idx = S.activeTraceIdx;
  const arr = S.rtaDisplays.slice();
  arr[idx] = null;
  S.rtaAvgN[idx] = 0;
  S.setRtaDisplays(arr);
  renderAll();
}

export function setTraceMode(mode: string) {
  const t = S.traces[S.activeTraceIdx];
  t.mode = mode;
  if (mode !== 'VIEW' && mode !== t.prevMode) t.prevMode = mode;
  if (mode === 'OFF' || mode === 'CLEAR_WRITE' || mode === 'AVERAGE') {
    t.avgSum = null; t.avgCount = 0;
  }
  if (mode === 'OFF') { resetTraceAccum(t); }
  if (mode === 'MAX_HOLD' || mode === 'MIN_HOLD' || mode === 'AVERAGE' || mode === 'CLEAR_WRITE') {
    t.powers = null; t.avgSum = null; t.avgCount = 0;
  }
  syncFreezeBtn();
}
