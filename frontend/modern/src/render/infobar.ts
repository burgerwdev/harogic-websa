// Top info bar + device bar
import * as S from '../core/store';
import { formatBWHz } from '../core/fmt';
import { t } from '../core/i18n';

// SWT field throttling: the frame-header sweep_ms may vary slightly per frame (EMA convergence jitter),
// and high-frequency refresh would make the top bar flicker → update at most every 500ms
let lastSwtStr = '', lastSwtAt = 0;
const SWT_THROTTLE_MS = 500;
function setSwt() {
  const now = performance.now();
  if (now - lastSwtAt < SWT_THROTTLE_MS) return;   // skip entirely within the throttle window (regardless of whether the value changed)
  lastSwtAt = now;
  const v = S.sweepMs > 0 ? S.sweepMs.toFixed(1) + ' ms' : '-';
  if (v === lastSwtStr) return;                     // also skip if the value didn't change (avoid pointless writes)
  lastSwtStr = v;
  const el = document.getElementById('info-swt');
  if (el) el.textContent = v;
}

export function updateInfoBar() {
  const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  // ref display = display reference (displayRef) — ref level is a pure frontend display parameter
  set('info-ref', S.displayUnit === 'dB' ? S.displayRef.toFixed(1) + ' dB' : S.displayRef.toFixed(1) + ' dBm');
  set('info-scale', S.dbPerDiv + ' dB/div');
  set('info-rbw', formatBWHz(S.currentRBW) + (S.rbwMode === 'auto' ? ' (auto)' : ''));
  set('info-vbw', S.vbwMode === 'bypass' ? 'Bypass' : formatBWHz(S.currentVBW));
  setSwt();
  const dot = document.getElementById('connDot');
  if (dot) dot.className = 'dot ' + (S.deviceConnected ? 'on' : 'off');
  const bc = document.getElementById('btn-connect');
  if (bc) {
    bc.textContent = S.deviceConnected ? t('status_connected') : t('connect');
    bc.classList.toggle('active', !!S.deviceConnected);
  }
}
