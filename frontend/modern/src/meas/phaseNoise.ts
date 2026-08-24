// 相噪(PNM) 测量: 渲染/表格/结果处理
import * as S from '../core/store';
import { fmtHzUnit, formatFreqHz, fmtPnmFreq } from '../core/fmt';
import { renderAll } from '../render/spectrum';
import { send } from '../core/wsSend';
import { applyMeasUI } from '../ui/measure';
import { canvasColors } from '../core/theme';

// 首次完整采集后不再显示 measuring overlay(后端 progress 每轮循环, 测量持续刷新)
let pnmFirstDone = false;

export function measPnmApply() {
  pnmFirstDone = false;   // 重新测量: 重新显示 loading
  if (!S.measOn || S.measTabSel !== 'pnm') return;
  const center = parsePnmFreq() || 1e9;
  const thr = parseFloat((document.getElementById('input-pnm-thr') as HTMLInputElement).value);
  const avg = parseInt((document.getElementById('input-pnm-avg') as HTMLInputElement).value) || 4;
  if (S.viewMode !== 'pnm') {
    S.setViewMode('pnm');
    S.setPnmData(null);
    S.setPnmCarAcc(null);
    send({ cmd: 'SET_MODE', mode: 'pnm' });
  }
  send({ cmd: 'SET_PNM', center, threshold: isFinite(thr) ? thr : -50, traceavg: avg, start: 100, stop: 10e6 });
  applyMeasUI();
}

function parsePnmFreq(): number {
  const el = document.getElementById('input-pnm') as HTMLInputElement;
  const v = parseFloat(el?.value || '') || 0;
  const u = S.units.pnm;
  return u === 'GHz' ? v * 1e9 : u === 'MHz' ? v * 1e6 : u === 'kHz' ? v * 1e3 : v;
}

// 相噪图"完整"判据:
//  1. x 轴 label 齐全(offset 覆盖 100Hz~10MHz)
//  2. 曲线数据有效点占比 ≥90% (设备增量累积, 未更新段 pn=0 → 无效)
function pnmReady(d: any): boolean {
  if (!d || !d.offset || d.offset.length < 2 || !d.pn || d.pn.length < 2) return false;
  const offs = d.offset;
  if (!(offs[0] <= 100.5 && offs[offs.length - 1] >= 9.5e6)) return false;
  const pn = d.pn;
  // 图"真正画完" = 无无效点(-500 未更新段); 对应后端一轮增量采集完成(done)
  for (const v of pn) if (!isFinite(v) || v <= -250) return false;
  return true;
}

export function onPnmResult(d: any) {
  S.setPnmData(d);
  // 以 x 轴 label 齐全(offset 覆盖全范围)为准 dismiss —— 图真正画完才消失
  if (d.done || pnmReady(d)) {
    pnmFirstDone = true;
  }
  if (d.carrier_freq > 0 && isFinite(d.carrier_power) && d.carrier_power > -200 && d.carrier_power < 100) {
    if (!S.pnmCarAcc) S.setPnmCarAcc({ f: [], p: [], sumF: 0, sumP: 0 });
    S.pnmCarAcc.f.push(d.carrier_freq);
    S.pnmCarAcc.p.push(d.carrier_power);
    S.pnmCarAcc.sumF += d.carrier_freq;
    S.pnmCarAcc.sumP += d.carrier_power;
    if (S.pnmCarAcc.f.length > 30) {
      S.pnmCarAcc.sumF -= S.pnmCarAcc.f.shift();
      S.pnmCarAcc.sumP -= S.pnmCarAcc.p.shift();
    }
  }
  renderAll();
}

function pnmSmoothOn(): boolean {
  const el = document.getElementById('select-pnm-smooth') as HTMLSelectElement;
  return el?.value === '1';
}
function pnmWinPct(): number {
  const v = parseFloat((document.getElementById('input-pnm-win') as HTMLInputElement).value);
  return isFinite(v) ? Math.max(0, Math.min(10, v)) : 0;
}
function smoothArr(arr: Float32Array, winPct: number): Float32Array {
  const n = arr.length;
  if (n < 3 || winPct <= 0) return arr;
  const w = Math.max(1, Math.round(n * winPct / 100));
  const pre = new Float64Array(n + 1);
  for (let i = 0; i < n; i++) pre[i + 1] = pre[i] + arr[i];
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    const a = Math.max(0, i - w), b = Math.min(n - 1, i + w);
    out[i] = (pre[b + 1] - pre[a]) / (b - a + 1);
  }
  return out;
}

function pnmAt(f: number): number | null {
  const d = S.pnmData;
  if (!d) return null;
  const offs = d.offset;
  const pns = pnmSmoothOn() ? smoothArr(d.pn, pnmWinPct()) : d.pn;
  if (!offs || !offs.length) return null;
  if (f < offs[0] || f > offs[offs.length - 1]) return null;
  if (f <= offs[0]) return pns[0];
  if (f >= offs[offs.length - 1]) return pns[pns.length - 1];
  let j = 0; while (j < offs.length - 2 && offs[j + 1] < f) j++;
  const t = (f - offs[j]) / (offs[j + 1] - offs[j]);
  return pns[j] + t * (pns[j + 1] - pns[j]);
}

export function renderPnm() {
  const c = document.getElementById('spectrum') as HTMLCanvasElement;
  const ctx2 = c.getContext('2d')!;
  const col = canvasColors();
  ctx2.clearRect(0, 0, c.width, c.height);
  const d = S.pnmData;
  // 测量中(无数据): 明确的 loading 提示 + 进度条, 避免用户误以为卡死
  if (!d || !d.offset || d.offset.length < 2) {
    drawLoading(c, ctx2, col, d && d.progress != null ? Math.min(100, d.progress) : 0);
    return;
  }
  const m = { l: 50, r: 60, t: 30, b: 40 };
  const Wd = c.width - m.l - m.r, Hd = c.height - m.t - m.b;
  if (d.progress != null) {
    const pr = Math.min(100, d.progress);
    ctx2.fillStyle = col.axis;
    ctx2.font = 'bold 11px monospace';
    ctx2.textAlign = 'left';
    ctx2.fillText(pr >= 100 ? 'measuring... updating' : 'measuring... ' + pr + '%', 4, 12);
    const pbH = 5, pbY = c.height - 14;
    ctx2.fillStyle = col.grid;
    ctx2.fillRect(m.l, pbY, Wd, pbH);
    ctx2.fillStyle = col.axis;
    ctx2.fillRect(m.l, pbY, Wd * pr / 100, pbH);
    ctx2.strokeStyle = col.grid;
    ctx2.strokeRect(m.l, pbY, Wd, pbH);
  }
  const fLo = d.offset[0], fHi = d.offset[d.offset.length - 1];
  const X = (f: number) => m.l + Math.log10(Math.max(f, fLo) / fLo) / Math.log10(fHi / fLo) * Wd;
  const pMin = -170, pMax = -60;
  const Y = (v: number) => m.t + (pMax - v) / (pMax - pMin) * Hd;
  ctx2.fillStyle = col.bg;
  ctx2.fillRect(0, 0, c.width, c.height);
  ctx2.strokeStyle = col.grid;
  ctx2.strokeRect(m.l, m.t, Wd, Hd);
  ctx2.fillStyle = col.axis;
  ctx2.font = '10px monospace';
  ctx2.strokeStyle = col.grid;
  for (let f = 100; f <= 1e7; f *= 10) {
    if (f < fLo) continue;
    const x = X(f);
    ctx2.beginPath(); ctx2.moveTo(x, m.t); ctx2.lineTo(x, m.t + Hd); ctx2.stroke();
    ctx2.textAlign = 'center'; ctx2.textBaseline = 'top';
    ctx2.fillText(fmtPnmFreq(f), x, m.t + Hd + 6);
  }
  ctx2.fillText('Offset', m.l + Wd / 2, m.t + Hd + 20);
  ctx2.strokeStyle = col.grid;
  ctx2.textAlign = 'right'; ctx2.textBaseline = 'middle';
  ctx2.font = '10px monospace';
  for (let v = Math.ceil(pMax / 20) * 20; v >= pMin; v -= 20) {
    const y = Y(v);
    ctx2.beginPath(); ctx2.moveTo(m.l, y); ctx2.lineTo(m.l + Wd, y); ctx2.stroke();
    ctx2.fillStyle = col.axis;
    ctx2.fillText(v + '', m.l - 6, y);
  }
  ctx2.fillStyle = col.axis; ctx2.textAlign = 'left'; ctx2.textBaseline = 'top';
  ctx2.fillText('dBc/Hz', m.l + 4, m.t + 4);
  const pnDisp = pnmSmoothOn() ? smoothArr(d.pn, pnmWinPct()) : d.pn;
  ctx2.strokeStyle = col.pnm;
  ctx2.lineWidth = 1;
  ctx2.beginPath();
  let pen = false;
  for (let i = 0; i < d.offset.length; i++) {
    const v = pnDisp[i];
    if (!isFinite(v) || v >= 0) { pen = false; continue; }   // 无效点(未更新=0)不连线
    const x = X(d.offset[i]), y = Y(v);
    if (!pen) { ctx2.moveTo(x, y); pen = true; } else ctx2.lineTo(x, y);
  }
  ctx2.stroke();
  ctx2.fillStyle = col.peak;
  for (const f of S.PNM_OFFSETS) {
    if (f < fLo || f > fHi) continue;
    const x = X(f), y = Y(pnmAt(f) ?? -170);
    ctx2.beginPath();
    ctx2.moveTo(x, y - 4); ctx2.lineTo(x + 3, y); ctx2.lineTo(x, y + 4); ctx2.lineTo(x - 3, y);
    ctx2.fill();
  }

  // 仅首次采集未完成时叠加 measuring 提示(此后 progress 循环不再打扰)
  if (!pnmFirstDone && d.progress != null && d.progress < 100) {
    drawProgressOverlay(c, ctx2, col, Math.min(100, d.progress));
  }
}

// 曲线绘制完成但仍在增量采集中: 半透明覆盖 + 中央进度提示
function drawProgressOverlay(c: HTMLCanvasElement, ctx2: CanvasRenderingContext2D, col: any, progress: number) {
  ctx2.save();
  ctx2.fillStyle = 'rgba(0, 0, 0, 0.45)';
  ctx2.fillRect(0, 0, c.width, c.height);
  const cx = c.width / 2, cy = c.height / 2 - 10;
  ctx2.fillStyle = col.text;
  ctx2.font = 'bold 15px monospace';
  ctx2.textAlign = 'center'; ctx2.textBaseline = 'middle';
  ctx2.fillText('MEASURING... ' + progress + '%', cx, cy - 20);
  const bw = 240, bh = 7, bx = cx - bw / 2, by = cy + 8;
  ctx2.fillStyle = col.grid;
  ctx2.fillRect(bx, by, bw, bh);
  ctx2.fillStyle = col.axis;
  ctx2.fillRect(bx, by, bw * progress / 100, bh);
  ctx2.strokeStyle = col.grid;
  ctx2.strokeRect(bx, by, bw, bh);
  ctx2.restore();
}

export function updatePnmTable() {
  const tb = document.getElementById('pnm-table');
  const tb2 = document.getElementById('pnm-tbody');
  if (!tb || !tb2) return;
  if (!S.pnmData || !S.pnmData.offset) { tb.style.display = 'none'; return; }
  tb.style.display = '';
  const acc = S.pnmCarAcc;
  const c = (acc && acc.f.length)
    ? { f: acc.sumF / acc.f.length, p: acc.sumP / acc.p.length }
    : null;
  const ready = !!c;
  const offHz = ready ? c.f - (parsePnmFreq() || 0) : 0;
  let html = '<tr><th style="width:30%;" colspan="2">Carrier</th></tr>' +
    '<tr><td>Frequency</td><td>' + (ready ? formatFreqHz(c.f) : 'measuring...') + '</td></tr>' +
    '<tr><td>Offset</td><td>' + (ready ? (offHz >= 0 ? '+' : '') + fmtHzUnit(Math.abs(offHz)) : '—') + '</td></tr>' +
    '<tr><td>Power</td><td>' + (ready ? c.p.toFixed(1) + ' dBm' : '—') + '</td></tr>' +
    '<tr><th colspan="2">Phase Noise</th></tr>';
  for (const f of S.PNM_OFFSETS) {
    const v = pnmAt(f);
    html += '<tr><td>' + fmtPnmFreq(f) + ' Hz</td><td>' + (v != null ? v.toFixed(1) + ' dBc/Hz' : '—') + '</td></tr>';
  }
  tb2.innerHTML = html;
}

// 中央 loading 提示 + 进度条
function drawLoading(c: HTMLCanvasElement, ctx2: CanvasRenderingContext2D, col: any, progress: number) {
  ctx2.fillStyle = col.bg;
  ctx2.fillRect(0, 0, c.width, c.height);
  const cx = c.width / 2, cy = c.height / 2;
  // 标题
  ctx2.fillStyle = col.text;
  ctx2.font = 'bold 16px monospace';
  ctx2.textAlign = 'center'; ctx2.textBaseline = 'middle';
  ctx2.fillText(progress >= 100 ? 'PHASE NOISE  (updating...)' : 'PHASE NOISE  (measuring... ' + progress + '%)', cx, cy - 24);
  // 进度条
  const bw = 280, bh = 8, bx = cx - bw / 2, by = cy + 12;
  ctx2.fillStyle = col.grid;
  ctx2.fillRect(bx, by, bw, bh);
  ctx2.fillStyle = col.axis;
  ctx2.fillRect(bx, by, bw * progress / 100, bh);
  ctx2.strokeStyle = col.grid;
  ctx2.lineWidth = 1;
  ctx2.strokeRect(bx, by, bw, bh);
  // 提示
  ctx2.fillStyle = col.axis;
  ctx2.font = '12px monospace';
  ctx2.fillText('please wait...', cx, by + 26);
}
