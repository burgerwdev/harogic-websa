// Export the spectrum plot as a PNG with a compact header of the acquisition settings.
//
// The header is intentionally a neutral data line (version / range / settings / time)
// rather than UI prose, so a screenshot is self-describing in a report.
import * as S from '../core/store';
import { formatFreqHz } from '../core/fmt';

function headerParts(): string[] {
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
  return parts;
}

// Greedy wrap of header segments into lines that fit the available width.
function wrapSegments(c: CanvasRenderingContext2D, parts: string[], maxWidth: number): string[] {
  const lines: string[] = [];
  let cur = '';
  for (const p of parts) {
    const cand = cur ? `${cur}  ${p}` : p;
    if (cur && c.measureText(cand).width > maxWidth) {
      lines.push(cur);
      cur = p;
    } else {
      cur = cand;
    }
  }
  if (cur) lines.push(cur);
  return lines;
}

export function exportSpectrumPng(): void {
  const src = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (!src) return;
  const pad = 12;
  const lineH = 16;
  const parts = headerParts();
  const out = document.createElement('canvas');
  out.width = src.width + pad * 2;
  const probe = out.getContext('2d');
  if (!probe) return;
  // The full header can be wider than the plot (version + settings + timestamp), so wrap it
  // and only shrink the font when two lines are still not enough.
  let font = 'bold 13px monospace';
  probe.font = font;
  let lines = wrapSegments(probe, parts, out.width - pad * 2);
  if (lines.length > 2) {
    font = 'bold 11px monospace';
    probe.font = font;
    lines = wrapSegments(probe, parts, out.width - pad * 2);
  }
  const head = 10 + lines.length * lineH;
  out.height = src.height + head + pad;   // resizing resets the context state
  const c = out.getContext('2d');
  if (!c) return;
  c.fillStyle = '#000000';
  c.fillRect(0, 0, out.width, out.height);
  c.fillStyle = '#00ff66';
  c.font = font;
  c.textBaseline = 'alphabetic';
  lines.forEach((l, i) => c.fillText(l, pad, 8 + (i + 1) * lineH - 5));
  c.drawImage(src, pad, head);
  const link = document.createElement('a');
  link.href = out.toDataURL('image/png');
  link.download = `websa_sa_${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
  link.click();
}
