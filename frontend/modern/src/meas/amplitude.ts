// Amplitude measurement: multi-threshold n-dB auto-location
import * as S from '../core/store';
import { getX, getY, renderAll } from '../render/spectrum';
import { getDisplayPowers } from '../dsp/peaks';
import { t } from '../core/i18n';
import { fmtF } from '../core/fmt';
import { canvasColors } from '../core/theme';
import { plotRect } from '../render/plot';

export function measureAmp() {
  const dp = getDisplayPowers();
  if (!dp || !S.freqArray || S.freqArray.length < 2) return;
  const inp = document.getElementById('input-ampdbs') as HTMLInputElement;
  const thrs = (inp?.value || '3,10,20')
    .split(',').map(s => parseFloat(s.trim()))
    .filter(v => isFinite(v) && v >= 1 && v <= 60);
  if (!thrs.length) { alert(t('meas_invalid_thresholds')); return; }
  let pi = 0, pv = -1e9;
  for (let i = 0; i < dp.length; i++) if (dp[i] > pv) { pv = dp[i]; pi = i; }
  const rows: { thr: number; lf: number; rf: number; bw: number; hasL: boolean; hasR: boolean }[] = [];
  const n = dp.length;
  const fa = S.freqArray;
  for (const thr of thrs) {
    const th = pv - thr;
    let li = pi; while (li > 0 && dp[li - 1] > th) li--;
    let ri = pi; while (ri < n - 1 && dp[ri + 1] > th) ri++;
    const hasL = !(li === 0 && dp[0] > th);
    const hasR = !(ri === n - 1 && dp[n - 1] > th);
    if (!hasL && !hasR) continue;
    const lf = hasL ? crossX(dp, li, th, -1) : fa[0];
    const rf = hasR ? crossX(dp, ri, th, +1) : fa[n - 1];
    rows.push({ thr, lf, rf, bw: rf - lf, hasL, hasR });
  }
  S.setAmpRes({ rows, peak: pv, pi });
  renderAll();
}

function crossX(powers: Float32Array, i: number, th: number, dir: number): number {
  const fa = S.freqArray!;
  const a = powers[i], b = powers[i + dir];
  if (a === b) return fa[i];
  const t = (th - a) / (b - a);
  return fa[i] + t * (fa[i + dir] - fa[i]);
}

export function clearAmp() { S.setAmpRes(null); renderAll(); }

export function renderAmp(powers: Float32Array) {
  const ar = S.ampRes;
  if (!ar || !ar.rows.length || !S.freqArray) return;
  const col = canvasColors();
  const p = plotRect();
  const c = S.ctx;
  c.save();
  c.beginPath(); c.rect(p.x, p.y, p.w, p.h); c.clip();
  ar.rows.forEach((r: any) => {
    const yT = getY(ar.peak - r.thr);
    c.strokeStyle = col.axis;
    c.setLineDash([5, 4]);
    c.beginPath(); c.moveTo(p.x, yT); c.lineTo(p.x + p.w, yT); c.stroke();
    c.setLineDash([]);
    c.fillStyle = col.axis;
    if (r.hasL) {
      const x = getXIdx(r.lf);
      c.beginPath();
      c.moveTo(x, yT - 5); c.lineTo(x + 4, yT); c.lineTo(x, yT + 5); c.lineTo(x - 4, yT);
      c.fill();
    }
    if (r.hasR) {
      const x = getXIdx(r.rf);
      c.beginPath();
      c.moveTo(x, yT - 5); c.lineTo(x + 4, yT); c.lineTo(x, yT + 5); c.lineTo(x - 4, yT);
      c.fill();
    }
    const lfTxt = r.hasL ? fmtF(r.lf) : 'edge';
    const rfTxt = r.hasR ? fmtF(r.rf) : 'edge';
    const bwTxt = (r.hasL && r.hasR) ? 'BW: ' + fmtF(r.bw) : 'BW: >' + fmtF(r.bw);
    const lbl = '-' + r.thr + 'dB / ' + lfTxt + ' ' + rfTxt + ' / ' + bwTxt;
    c.font = 'bold 10px monospace';
    const tw = c.measureText(lbl).width;
    const cx = p.x + p.w - tw / 2 - 6;
    const cy = yT - 6;
    c.fillStyle = col.labelBg;
    c.fillRect(cx - tw / 2 - 3, cy - 9, tw + 6, 12);
    c.fillStyle = col.axis;
    c.textAlign = 'center'; c.textBaseline = 'bottom';
    c.fillText(lbl, cx, cy);
  });
  c.restore();
}

function getXIdx(f: number): number {
  const p = plotRect();
  const fa = S.freqArray!;
  const n = fa.length;
  let i0 = 0, i1 = n - 1;
  for (let i = 0; i < n; i++) if (fa[i] >= f) { i1 = i; break; }
  if (fa[i1] === fa[i0]) return p.x;
  const t = (f - fa[i0]) / (fa[i1] - fa[i0]);
  return getX(i0 + t * (i1 - i0), n);
}
