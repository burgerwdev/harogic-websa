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
  return parts;
}

// Fixed-width local timestamp with the UTC offset (stable width, no locale surprises):
// 2026-09-10 15:52:18 UTC+08:00
function timeStamp(): string {
  const d = new Date();
  const p2 = (n: number) => String(n).padStart(2, '0');
  const offMin = -d.getTimezoneOffset();
  const sign = offMin >= 0 ? '+' : '-';
  const abs = Math.abs(offMin);
  return `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())} ` +
    `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())} ` +
    `UTC${sign}${p2(Math.floor(abs / 60))}:${p2(abs % 60)}`;
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
  const footH = 22;                        // reserved band so the timestamp never touches the plot
  out.height = src.height + head + footH;  // resizing resets the context state
  const c = out.getContext('2d');
  if (!c) return;
  c.fillStyle = '#000000';
  c.fillRect(0, 0, out.width, out.height);
  c.fillStyle = '#00ff66';
  c.font = font;
  c.textBaseline = 'alphabetic';
  lines.forEach((l, i) => c.fillText(l, pad, 8 + (i + 1) * lineH - 5));
  c.drawImage(src, pad, head);
  // Timestamp pinned to the bottom-right corner: same 12 px margin as the sides,
  // ~7 px below the plot, so it can never collide with the header or the trace area.
  c.font = 'bold 11px monospace';
  c.textAlign = 'right';
  c.fillText(timeStamp(), out.width - pad, out.height - 7);
  c.textAlign = 'left';
  const link = document.createElement('a');
  link.href = out.toDataURL('image/png');
  link.download = `websa_sa_${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
  link.click();
}
