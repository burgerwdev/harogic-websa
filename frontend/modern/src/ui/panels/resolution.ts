// Resolution panel: RBW / VBW / points / window / spur actions.
// Extracted from ui/controls.ts (report finding P1-5).
import * as S from '../../core/store';
import { parseFreqUnit } from '../../core/units';
import { send } from '../../core/wsSend';
import { clearRtaAccum } from './rta';
import { rbwMode, vbwMode } from '../swpState';

export function applyRBW() {
  const sel = document.getElementById('select-rbw-mode') as HTMLSelectElement;
  const mode = sel?.value || 'auto';
  rbwMode.set(mode);
  const m: any = { cmd: 'SET_RBW', mode };
  if (mode === 'manual') m.rbw = parseFreqUnit('rbw');
  if (S.rtaMode) clearRtaAccum();
  send(m);
}

export function applyVBW() {
  const sel = document.getElementById('select-vbw-mode') as HTMLSelectElement;
  const mode = sel?.value || 'bypass';
  vbwMode.set(mode);
  const m: any = { cmd: 'SET_VBW', mode };
  if (mode === 'manual') m.vbw = parseFreqUnit('vbw');
  if (S.rtaMode) clearRtaAccum();
  send(m);
}

export function applyPoints() {
  const el = document.getElementById('input-points') as HTMLInputElement;
  send({ cmd: 'SET_POINTS', points: parseInt(el?.value || '1001') || 1001 });
}

export function setSpurMode(mode: string) { send({ cmd: 'SET_SPUR', mode }); }

export function setWindow(v: string) { send({ cmd: 'SET_WINDOW', window: parseInt(v) }); }
