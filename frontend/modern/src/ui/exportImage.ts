// Export the spectrum plot as a PNG with a compact header of the acquisition settings.
//
// The header is intentionally a neutral data line (version / range / settings / time)
// rather than UI prose, so a screenshot is self-describing in a report.
import * as S from '../core/store';
import { formatFreqHz } from '../core/fmt';

function headerText(): string {
  const ver = (document.querySelector('.version-tag')?.textContent || '').trim().split(/\s+/)[0] || 'HAROGIC WebSA';
  const det = (document.getElementById('select-detector') as HTMLSelectElement | null)?.value || '';
  const n = S.freqArray?.length ?? S.currentPoints;
  const lo = S.centerHz - S.spanHz / 2;
  const hi = S.centerHz + S.spanHz / 2;
  const parts = [
    ver,
    `${formatFreqHz(lo)} - ${formatFreqHz(hi)}`,
    `Ref ${S.refLevel.toFixed(1)} dBm`,
    `${S.dbPerDiv.toFixed(0)} dB/div`,
    `RBW ${formatFreqHz(S.currentRBW)}`,
    `VBW ${formatFreqHz(S.currentVBW)}`,
    `${n} pts`,
  ];
  if (det) parts.push(`det ${det}`);
  parts.push(new Date().toLocaleString());
  return parts.join('  ');
}

export function exportSpectrumPng(): void {
  const src = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (!src) return;
  const pad = 12;
  const head = 30;
  const out = document.createElement('canvas');
  out.width = src.width + pad * 2;
  out.height = src.height + head + pad;
  const c = out.getContext('2d');
  if (!c) return;
  c.fillStyle = '#000000';
  c.fillRect(0, 0, out.width, out.height);
  c.fillStyle = '#00ff66';
  c.font = 'bold 13px monospace';
  c.textBaseline = 'alphabetic';
  c.fillText(headerText(), pad, 20);
  c.drawImage(src, pad, head);
  const link = document.createElement('a');
  link.href = out.toDataURL('image/png');
  link.download = `websa_sa_${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
  link.click();
}
