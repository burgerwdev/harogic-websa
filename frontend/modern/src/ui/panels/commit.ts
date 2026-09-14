// Unit-field commit: maps an edited numeric field to the panel action that applies it.
// Its own module because it calls into three panels (report finding P1-5).
import * as S from '../../core/store';
import { measPnmApply } from '../../meas/phaseNoise';
import { applyCenterSpan, applyStartStop, syncSwpSpanStep } from './frequency';
import { applyRBW, applyVBW } from './resolution';
import { applyRta } from './rta';

export function commitUnitField(field: string, commit = true) {
  if (field === 'span') syncSwpSpanStep();
  if (!commit) return;
  if (field === 'center' || field === 'span') {
    applyCenterSpan();
  } else if (field === 'start' || field === 'stop') {
    applyStartStop();
  } else if (field === 'rta_center') {
    applyRta();
  } else if (field === 'rbw') {
    const mode = document.getElementById('select-rbw-mode') as HTMLSelectElement | null;
    if (mode) mode.value = 'manual';
    applyRBW();
  } else if (field === 'vbw') {
    const mode = document.getElementById('select-vbw-mode') as HTMLSelectElement | null;
    if (mode) mode.value = 'manual';
    applyVBW();
  } else if (field === 'pnm' && S.measOn && S.measTabSel === 'pnm') {
    measPnmApply();
  }
}
