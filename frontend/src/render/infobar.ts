// Top info bar + device bar
import * as S from '../core/store';
import { formatBWHz } from '../core/fmt';
import { t } from '../core/i18n';
import { rbwMode, vbwMode, currentRBW, currentVBW } from '../ui/swpState';
import { getDisplayRef } from '../ui/displayRef';
import { displayOffset, displayUnit } from '../ui/displayState';
import { effectiveRefLevel } from '../ui/refState';

// SWT prefers the device's estimated minimum sweep time. Some SDK profiles report zero;
// in that case show the publisher's measured step duration.
// The quantities remain separate and the display source changes only with SDK availability.
let lastSwtStr = '', lastSwtAt = 0, lastSwtMode = '';
const SWT_THROTTLE_MS = 500;
function setSwt() {
  const now = performance.now();
  const mode = S.rtaMode ? 'rta' : 'std';
  const modeChanged = mode !== lastSwtMode;
  if (!modeChanged && now - lastSwtAt < SWT_THROTTLE_MS) return;
  lastSwtMode = mode;
  lastSwtAt = now;
  // RTA mode: show current RTA bandwidth (start~stop) instead of sweep time
  let v: string;
  if (S.rtaMode && S.rtaData && S.rtaData.stopHz) {
    v = formatBWHz(S.rtaData.stopHz - S.rtaData.startHz);
  } else {
    const ms = S.sweepMs > 0 ? S.sweepMs : S.sweepMsMeas;
    v = ms > 0 ? ms.toFixed(1) + ' ms' : '-';
  }
  if (v === lastSwtStr) return;                     // also skip if the value didn't change (avoid pointless writes)
  lastSwtStr = v;
  const el = document.getElementById('info-swt');
  if (el) el.textContent = v;
}

export function updateInfoBar() {
  const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  // RTA mode: the SWT field shows the RTA analysis bandwidth -> label becomes "BW:"
  const swtLabel = document.querySelector('[data-i18n="swt"]');
  if (swtLabel) swtLabel.textContent = S.rtaMode ? t('bw_label') : t('swt');
  // Ref retains the compensated reference setting while zoom has independent display bounds.
  const ref = getDisplayRef();
  set('info-ref', ref.toFixed(1) + (displayUnit.get() === 'dB' ? ' dB' : ' dBm'));
  const hardwareRef = effectiveRefLevel.confirmedValue();
  if (hardwareRef !== null) {
    const detail = t('ref_hardware_detail', {
      display: ref.toFixed(1), offset: displayOffset.get().toFixed(1), actual: hardwareRef.toFixed(1),
    });
    for (const id of ['info-ref', 'input-ref']) {
      const element = document.getElementById(id);
      if (element) element.title = detail;
    }
  }
  set('info-scale', `${Number(S.dbPerDiv.toFixed(2))} ${t('db_per_div')}`);
  set('info-rbw', formatBWHz(currentRBW.get()) + (rbwMode.get() === 'auto' ? ' (auto)' : ''));
  set('info-vbw', vbwMode.get() === 'bypass' ? 'Bypass' : formatBWHz(currentVBW.get()));
  setSwt();
  const dot = document.getElementById('connDot');
  if (dot) dot.className = 'dot ' + (S.deviceConnected ? 'on' : 'off');
  const bc = document.getElementById('btn-connect');
  if (bc) {
    bc.textContent = S.deviceConnected ? t('status_connected') : t('connect');
    bc.classList.toggle('active', !!S.deviceConnected);
  }
}
