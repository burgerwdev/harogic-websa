// 谐波标注线(真实频谱上) renderHarmOverlay
import * as S from '../core/store';
import { getX } from '../render/spectrum';
import { plotRect } from '../render/plot';

export function renderHarmOverlay(powers: Float32Array) {
  const h = S.harm;
  if (!h || !h.list || !S.freqArray) return;
  const col = S.ctx;
  const p = plotRect(), n = powers.length;
  const fa = S.freqArray;
  col.save();
  col.beginPath(); col.rect(p.x, p.y, p.w, p.h); col.clip();
  col.font = 'bold 10px monospace';
  h.list.forEach((hh: any) => {
    if (!isFinite(hh.f) || !isFinite(hh.amp)) return;
    if (hh.f < fa[0] || hh.f > fa[fa.length - 1]) return;
    let best = 0, bd = 1e30;
    for (let i = 0; i < fa.length; i++) {
      const d = Math.abs(fa[i] - hh.f);
      if (d < bd) { bd = d; best = i; }
    }
    const x = getX(best, n);
    const isFund = hh.n === 1;
    col.strokeStyle = isFund ? '#00ff88' : '#0088ff';
    col.globalAlpha = 0.6;
    col.setLineDash([4, 4]);
    col.lineWidth = isFund ? 1.6 : 1;
    col.beginPath(); col.moveTo(x, p.y); col.lineTo(x, p.y + p.h); col.stroke();
    col.setLineDash([]);
    const label = 'H' + hh.n + (isFund ? ' ' + hh.amp.toFixed(1) + 'dBm' : ' ' + hh.dbc.toFixed(1) + 'dBc');
    const tw = col.measureText(label).width;
    let tx = x - tw / 2;
    tx = Math.max(p.x + 2, Math.min(p.x + p.w - tw - 2, tx));
    col.globalAlpha = 1;
    col.fillStyle = hlb();
    col.fillRect(tx, p.y + 2, tw + 4, 12);
    col.strokeStyle = hlb2();
    col.lineWidth = 0.8;
    col.strokeRect(tx, p.y + 2, tw + 4, 12);
    col.fillStyle = isFund ? '#00ff88' : '#44ccff';
    col.textAlign = 'left'; col.textBaseline = 'bottom';
    col.fillText(label, tx + 2, p.y + 14);
  });
  col.restore();
}

function hlb() { return (document.documentElement.dataset.theme === 'light') ? 'rgba(255,255,255,0.88)' : 'rgba(0,0,0,0.75)'; }
function hlb2() { return (document.documentElement.dataset.theme === 'light') ? '#b0c4b0' : '#00aa66'; }
