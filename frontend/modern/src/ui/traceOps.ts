// 迹线 UI 操作: tab 切换 / 模式设置
import * as S from '../core/store';
import { updateInfoBar } from '../render/infobar';
import { updateNormalizeStatusUI } from '../dsp/normalize';
import { resetTraceAccum } from '../dsp/traces';

export function switchTraceTab(idx: number) {
  S.setActiveTraceIdx(idx);
  document.querySelectorAll('.trace-btn').forEach(b => b.classList.remove('active'));
  const btns = document.querySelectorAll('.trace-btn');
  if (btns[idx]) btns[idx].classList.add('active');
  const t = S.traces[idx];
  const sel = document.getElementById('select-trace-mode') as HTMLSelectElement;
  if (sel) sel.value = t.mode;
  const anyNorm = S.traces.some(x => x.isNormalized && x.reference);
  S.setDisplayUnit((t.reference && t.isNormalized) ? 'dB' : (anyNorm ? 'dB' : 'dBm'));
  S.setDisplayRef(S.displayUnit === 'dB' ? 0.0 : S.refLevel);
  updateNormalizeStatusUI();
  updateInfoBar();
}

export function setTraceMode(mode: string) {
  const t = S.traces[S.activeTraceIdx];
  t.mode = mode;
  if (mode === 'OFF' || mode === 'CLEAR_WRITE' || mode === 'AVERAGE') {
    t.avgSum = null; t.avgCount = 0;
  }
  if (mode === 'OFF') { resetTraceAccum(t); }
  if (mode === 'MAX_HOLD' || mode === 'MIN_HOLD' || mode === 'AVERAGE' || mode === 'CLEAR_WRITE') {
    t.powers = null; t.avgSum = null; t.avgCount = 0;
  }
}
