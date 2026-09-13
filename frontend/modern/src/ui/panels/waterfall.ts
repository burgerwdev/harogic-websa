// Waterfall controls and the sweep-speed field.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { t } from '../../core/i18n';
import { send } from '../../core/wsSend';
import { requestRender } from '../../render/redraw';
import { waterfallOn, wfPaused } from '../waterfallState';

export function toggleWaterfall() {
  waterfallOn.set(!waterfallOn.get());
  if (waterfallOn.get()) S.resetWaterfall();
  const wf = document.getElementById('waterfall');
  if (wf) wf.style.display = waterfallOn.get() ? '' : 'none';
  const mt = document.getElementById('marker-table');
  if (mt) mt.style.display = waterfallOn.get() ? 'none' : '';
  const btn = document.getElementById('btn-waterfall');
  if (btn) btn.classList.toggle('active', waterfallOn.get());   // text stays "Waterfall", active = on
  requestRender();
}

export function toggleWfPause() {
  wfPaused.set(!wfPaused.get());
  const b = document.getElementById('btn-wf-pause');
  if (b) b.classList.toggle('active', wfPaused.get());
}

export function resetWf() {
  S.resetWaterfall();
  requestRender();
}

export function setSweepSpeed() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const mode = parseInt(sel?.value || '0');
  const time = parseFloat(tin?.value || '0');
  const m: any = { cmd: 'SET_SWEEP', mode };
  if (mode === 6 || mode === 7 || mode === 8) {
    const minimum = mode === 7 ? 0.001 : 1;
    m.time = Math.max(minimum, isFinite(time) ? time : minimum);
  }
  send(m);
}

export function syncSweepInput() {
  const sel = document.getElementById('select-sweep-mode') as HTMLSelectElement;
  const tin = document.getElementById('input-sweep-time') as HTMLInputElement;
  const btn = document.querySelector('button[data-action="set-sweep"]') as HTMLElement;
  if (!sel) return;
  const mode = parseInt(sel.value || '0');
  const need = mode >= 6;   // minSWTxN(6)/Manual(7)/minSMPxN(8) need an input value
  if (tin) { tin.style.display = need ? '' : 'none'; tin.placeholder = mode === 7 ? t('swt_sec') : t('swt_xn'); }
  if (btn) btn.style.display = need ? '' : 'none';
}
