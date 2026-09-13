/**
 * Frequency-editor refresh (leaf).
 *
 * `core/ws.ts` calls it when a STATUS reports new frequency values and `ui/controls.ts`
 * calls it on user actions; living in either module created a cycle between them
 * (docs/.../ARCH_REVIEW.md finding P1-4).
 */
import * as S from '../core/store';
import { toUnit } from '../core/units';
import { centerHz, rtaCenterHz, spanHz } from './freqState';
import { currentRBW, currentVBW } from './swpState';

function setInput(id: string, v: string, force = false) {
  const el = document.getElementById(id) as HTMLInputElement;
  if (el && (force || document.activeElement !== el)) el.value = v;
}

export function updateFreqUIInputs(force = false) {
  const swpEditor = document.getElementById('swp-freq-settings');
  if (swpEditor?.dataset.dirty !== '1') {
    setInput('input-center', toUnit(centerHz.get(), 'center').toFixed(4), force);
    setInput('input-span', toUnit(spanHz.get(), 'span').toFixed(4), force);
    setInput('input-start', toUnit(centerHz.get() - spanHz.get() / 2, 'start').toFixed(4), force);
    setInput('input-stop', toUnit(centerHz.get() + spanHz.get() / 2, 'stop').toFixed(4), force);
  }
  setInput('input-rbw', toUnit(currentRBW.get(), 'rbw').toFixed(2));
  setInput('input-vbw', toUnit(currentVBW.get(), 'vbw').toFixed(2));
  const rtaEditor = document.getElementById('rta-freq-settings');
  if (S.rtaMode && rtaEditor?.dataset.dirty !== '1') {
    const u = S.units.rta_center || 'MHz';
    const scale = u === 'GHz' ? 1e9 : u === 'kHz' ? 1e3 : 1e6;
    setInput('input-rta-center', (rtaCenterHz.get() / scale).toFixed(4), force);
  }
}
