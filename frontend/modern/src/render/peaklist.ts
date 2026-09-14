// Pk list: peak detection/threshold/table/on-canvas marks
import * as S from '../core/store';
import { fmtLevel } from '../core/level';
import { getX, getY } from './plot';
import { fmtF } from '../core/fmt';
import { t } from '../core/i18n';
import { percentileApprox } from '../dsp/stats';
import { centerHz, spanHz } from '../ui/freqState';
import { rbwMode, currentRBW, currentPoints } from '../ui/swpState';
import { refLevel } from '../ui/refState';

export function peakListOn(): boolean { return peakListVisible.get(); }

export function togglePeakList() {
  peakListVisible.set(!peakListVisible.get());
  const btn = document.getElementById('btn-peaklist');
  if (btn) btn.textContent = peakListVisible.get() ? t('on') : t('off');
  if (peakListVisible.get()) requestRender();
  requestRender();
}

export function findPeaks(powers: Float32Array, maxN: number): { idx: number; amp: number; f: number }[] {
  const thr = peakThr.get();
  const fa = S.freqArray;
  const peaks: { idx: number; amp: number; f: number }[] = [];
  for (let i = 1; i < powers.length - 1; i++) {
    const v = powers[i];
    if (v > thr && v >= powers[i - 1] && v > powers[i + 1]) {
      peaks.push({ idx: i, amp: v, f: fa ? fa[i] : i });
    }
  }
  peaks.sort((a, b) => a.idx - b.idx);
  const out: typeof peaks = [];
  for (const pk of peaks) {
    let replaced = false;
    for (let j = 0; j < out.length; j++) {
      if (Math.abs(out[j].idx - pk.idx) < 8 && Math.abs(out[j].amp - pk.amp) < 10) {
        if (pk.amp > out[j].amp) out[j] = pk;
        replaced = true; break;
      }
    }
    if (!replaced) {
      out.push(pk);
      if (out.length >= maxN) break;
    }
  }
  return out;
}

/** Frames medianed into one auto value, and the hysteresis that keeps it from twitching. */
const AUTO_SAMPLES = 5;
const AUTO_HYSTERESIS_DB = 2;

/** The measurement geometry the current auto threshold was fitted for. */
let fittedKey = '';
let samples: number[] = [];

function thrInput(): HTMLInputElement | null {
  return document.getElementById('input-peakthr') as HTMLInputElement | null;
}

/** The slot owns the value; the input element is only its projection. */
function setPeakThr(value: number): void {
  peakThr.set(value);
  const el = thrInput();
  if (el && document.activeElement !== el) el.value = String(Math.round(value));
}

/** Geometry that moves the trace (and with it the sensible threshold). */
function geometryKey(): string {
  return [
    centerHz.get(), spanHz.get(), currentRBW.get(), rbwMode.get(), refLevel.get(),
    S.dbPerDiv, S.totalDivs, currentPoints.get(),
  ].join('|');
}

/** Robust per-frame peak: the top 0.5 % of bins, so a single-bin spike cannot own it. */
function robustPeak(powers: Float32Array): number {
  return percentileApprox(powers, 0.995);
}

function commitAutoThr(robust: number): boolean {
  if (!isFinite(robust) || robust <= -200) return false;
  const thr = Math.round(robust - 50);
  if (Math.abs(thr - peakThr.get()) < AUTO_HYSTERESIS_DB) return false;
  setPeakThr(thr);
  return true;
}

/**
 * Auto threshold: ONE decision per measurement geometry.
 *
 * It used to be recomputed every frame from the instantaneous global peak, so the threshold -
 * which decides peak-table membership and marker peak search - moved with every amplitude
 * change and reshuffled both. Now the value is latched: it is fitted once when the geometry
 * changes (span/RBW/Ref/dB-per-div/points/centre move the trace), from the median of a few
 * frames so a settling frame cannot set it, and a manual edit still owns it until Auto is
 * pressed again.
 */
export function autoPeakThr(powers: Float32Array | null) {
  if (peakThrUserSet.get()) return;
  if (!powers) return;
  if (!S.markers.some(m => m.enabled)) return;
  const key = geometryKey();
  if (key !== fittedKey) {
    fittedKey = key;
    samples = [];
  }
  if (samples.length >= AUTO_SAMPLES) return;      // already fitted for this geometry
  const el = thrInput();
  if (el && document.activeElement === el) return;
  samples.push(robustPeak(powers));
  if (samples.length < AUTO_SAMPLES) return;
  const sorted = [...samples].sort((a, b) => a - b);
  commitAutoThr(sorted[sorted.length >> 1]);
}

export function peakThrManual() {
  const el = thrInput();
  const v = el ? parseFloat(el.value) : NaN;
  if (isFinite(v)) peakThr.set(v);
  peakThrUserSet.set(true);
  requestRender();
}

/** The user pressed Auto: fit once, now, from the trace they are looking at. */
export function peakThrAuto() {
  peakThrUserSet.set(false);
  fittedKey = '';
  samples = [];
  const dp = getDisplayPowers();
  if (dp) commitAutoThr(robustPeak(dp));
  requestRender();
}

/** Factory defaults: back to the automatic value (used by Preset). */
export function resetPeakThr() {
  peakThrUserSet.set(false);
  fittedKey = '';
  samples = [];
  setPeakThr(-80);
}

export function updatePeakTable(powers: Float32Array | null) {
  const tb = document.getElementById('peak-table');
  const tb2 = document.getElementById('peak-tbody');
  if (!tb || !tb2) return;
  if (!peakListVisible.get() || !powers || !S.freqArray) { tb.style.display = 'none'; return; }
  const cntEl = document.getElementById('input-peakcnt') as HTMLInputElement;
  const maxN = Math.max(1, Math.min(20, cntEl ? (parseInt(cntEl.value) || 20) : 20));
  const peaks = findPeaks(powers, maxN);
  S.setPeakMarks(peaks.map((pk, i) => Object.assign({}, pk, { n: i + 1 })));
  tb.style.display = '';
  const maxCols = 5, maxRows = 4;
  let html = '';
  for (let r = 0; r < maxRows; r++) {
    let cells = '';
    for (let c = 0; c < maxCols; c++) {
      const idx = r * maxCols + c;
      const pk = idx < peaks.length ? peaks[idx] : null;
      if (!pk) continue;
      cells += '<td class="pk-cell" title="Peak P' + (idx + 1) + '">' +
        '<span class="pk-id">P' + (idx + 1) + ':</span> ' +
        '<span class="pk-f">' + fmtF(pk.f) + '</span> / ' +
        '<span class="pk-a">' + fmtLevel(pk.amp, 1) + '</span></td>';
    }
    if (cells) html += '<tr>' + cells + '</tr>';
  }
  tb2.innerHTML = html;
}

export function renderPeakMarks(powers: Float32Array) {
  if (!peakListVisible.get() || !S.peakMarks || !S.freqArray) return;
  const col = getColors();
  const p = plotRectLocal();
  const n = powers.length;
  ctx2.save();
  ctx2.beginPath(); ctx2.rect(p.x, p.y, p.w, p.h); ctx2.clip();
  ctx2.font = 'bold 9px monospace';
  S.peakMarks!.forEach((pk) => {
    if (pk.idx < 0 || pk.idx >= n) return;
    const x = getX(pk.idx, n);
    const y = getY(pk.amp);
    const yd = y - 10;
    ctx2.fillStyle = col.peak;
    ctx2.beginPath();
    ctx2.moveTo(x, yd - 4); ctx2.lineTo(x + 3, yd); ctx2.lineTo(x, yd + 4); ctx2.lineTo(x - 3, yd);
    ctx2.closePath(); ctx2.fill();
    const lbl = 'P' + pk.n;
    const tw = ctx2.measureText(lbl).width;
    let ly = yd - 6;
    if (ly < p.y + 8) ly = yd + 10;
    ctx2.fillStyle = col.labelBg;
    ctx2.fillRect(x - tw / 2 - 2, ly - 7, tw + 4, 10);
    ctx2.fillStyle = col.peak;
    ctx2.textAlign = 'center'; ctx2.textBaseline = 'bottom';
    ctx2.fillText(lbl, x, ly);
  });
  ctx2.restore();
}

import { ctx as ctx2, W as _W, H as _H, MARGIN as _M } from '../core/store';
import { plotRect as plotRectLocal } from './plot';
import { canvasColors as getColors } from '../core/theme';
import { getDisplayPowers } from '../dsp/peaks';
import { requestRender } from './redraw';
import { peakListVisible, peakThr, peakThrUserSet } from '../ui/measurePrefs';
