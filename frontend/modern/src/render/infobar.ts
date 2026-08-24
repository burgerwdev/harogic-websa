// 顶部信息条 + 设备条
import * as S from '../core/store';
import { formatBWHz } from '../core/fmt';
import { t } from '../core/i18n';

export function updateInfoBar() {
  const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  set('info-ref', S.displayUnit === 'dB' ? S.displayRef.toFixed(1) + ' dB' : S.refLevel.toFixed(1) + ' dBm');
  set('info-scale', S.dbPerDiv + ' dB/div');
  set('info-rbw', formatBWHz(S.currentRBW) + (S.rbwMode === 'auto' ? ' (auto)' : ''));
  set('info-vbw', S.vbwMode === 'bypass' ? 'Bypass' : formatBWHz(S.currentVBW));
  set('info-swt', S.sweepMs > 0 ? S.sweepMs.toFixed(1) + ' ms' : '-');
  const dot = document.getElementById('connDot');
  if (dot) dot.className = 'dot ' + (S.deviceConnected ? 'on' : 'off');
  const bc = document.getElementById('btn-connect');
  if (bc) {
    bc.textContent = S.deviceConnected ? t('status_connected') : t('connect');
    bc.classList.toggle('active', !!S.deviceConnected);
  }
}
