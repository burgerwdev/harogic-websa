// Canvas overlay: vertical lines + diamonds + labels for each harmonic — renderHarmonics
import * as S from '../core/store';
import { getX, getY } from '../render/spectrum';
import { plotRect } from '../render/plot';

export function renderHarmonics(powers: Float32Array) {
  const h = S.harm;
  if (!h || !h.list.length || !S.freqArray) return;
  const c = S.ctx;
  const p = plotRect(), n = powers.length;
  const fa = S.freqArray;
  c.save();
  c.beginPath(); c.rect(p.x, p.y, p.w, p.h); c.clip();
  c.font = 'bold 10px monospace';
  h.list.forEach((hh: any) => {
    if (hh.inSpan === false) return;
    let hi = hh.idx;
    if (hi == null && fa) {
      let best = 0, bd = 1e30;
      for (let i = 0; i < fa.length; i++) {
        const d = Math.abs(fa[i] - hh.f);
        if (d < bd) { bd = d; best = i; }
      }
      hi = best;
    }
    if (hi == null || hi < 0 || !isFinite(hh.amp)) return;
    const x = getX(hi, n), y = getY(hh.amp);
    const isFund = hh.n === 1;
    c.strokeStyle = isFund ? '#00ff88' : '#0088ff';
    c.globalAlpha = isFund ? 0.8 : 0.5;
    c.setLineDash([4, 4]);
    c.lineWidth = 1;
    c.beginPath(); c.moveTo(x, p.y); c.lineTo(x, p.y + p.h); c.stroke();
    c.setLineDash([]);
    c.globalAlpha = 1;
    c.fillStyle = isFund ? '#00ff88' : '#00bbff';
    c.beginPath();
    c.moveTo(x, y - 5); c.lineTo(x + 4, y); c.lineTo(x, y + 5); c.lineTo(x - 4, y);
    c.fill();
    if (!isFinite(hh.dbc)) return;
    const label = 'H' + hh.n + (isFund ? ' ' + hh.amp.toFixed(1) + 'dBm' : ' ' + hh.dbc.toFixed(1) + 'dBc');
    const tw = c.measureText(label).width;
    let tx = x + 6;
    tx = Math.max(p.x + 2, Math.min(p.x + p.w - tw - 4, tx));
    const ty = Math.max(p.y + 14, Math.min(p.y + p.h - 8, y - 8));
    c.fillStyle = hlb();
    c.fillRect(tx - 2, ty - 9, tw + 4, 12);
    c.strokeStyle = hlb2();
    c.lineWidth = 0.8;
    c.strokeRect(tx - 2, ty - 9, tw + 4, 12);
    c.fillStyle = isFund ? '#00ff88' : '#44ccff';
    c.textAlign = 'left'; c.textBaseline = 'bottom';
    c.fillText(label, tx, ty);
  });
  c.restore();
}

function hlb() { return (document.documentElement.dataset.theme === 'light') ? 'rgba(255,255,255,0.88)' : 'rgba(0,0,0,0.7)'; }
function hlb2() { return (document.documentElement.dataset.theme === 'light') ? '#b0c4b0' : '#00aa66'; }
