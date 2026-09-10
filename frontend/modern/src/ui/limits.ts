// Limit-line UI: editable limit points, pass/fail status, persistence and CSV export.
//
// The canvas overlay is drawn in render/spectrum.ts; this module owns the panel,
// the persisted state and the DOM status line (so it stays free of canvas code).
import * as S from '../core/store';
import { applyI18n, onLangChange, t } from '../core/i18n';
import { buildLimitArray, normalizePoints, type LimitPoint } from '../dsp/limits';
import { getDisplayPowers } from '../dsp/peaks';
import { renderAll } from '../render/spectrum';
import { toDisplayLevel, fromDisplayLevel } from '../core/level';

const LS_KEY = 'websa-limits';
const DEFAULT_LEVEL_DROP = 20;   // dB below the reference level for the initial flat limit

function fmtMHz(hz: number): string {
  return (Math.round((hz / 1e6) * 1000) / 1000).toString();
}

/** Sweep edges clamped to the device range. */
export function spanEdges(): { start: number; stop: number } {
  const start = Math.max(S.FREQ_MIN, S.centerHz - S.spanHz / 2);
  const stop = Math.min(S.FREQ_MAX, S.centerHz + S.spanHz / 2);
  return { start, stop: stop > start ? stop : start + 1e6 };
}

/** Flat two-point limit across the current span. */
export function spanLimitPoints(): LimitPoint[] {
  const { start, stop } = spanEdges();
  const level = Math.round(S.refLevel - DEFAULT_LEVEL_DROP);
  return [{ freqHz: start, level }, { freqHz: stop, level }];
}

function load(): void {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) {
      S.setLimits({ on: false, tol: 0, points: spanLimitPoints() });
      return;
    }
    const d = JSON.parse(raw) as { on?: boolean; tol?: number; points?: LimitPoint[] };
    const pts = Array.isArray(d?.points) ? normalizePoints(d.points) : [];
    S.setLimits({
      on: !!d?.on,
      tol: Number.isFinite(d?.tol) ? Math.max(0, Number(d.tol)) : 0,
      points: pts.length ? pts : spanLimitPoints(),
    });
  } catch {
    S.setLimits({ on: false, tol: 0, points: spanLimitPoints() });   // corrupt state: fall back to defaults
  }
}

function save(): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify(S.limits));
  } catch {
    /* storage unavailable: limits still work for this session */
  }
}

function syncToggle(): void {
  const btn = document.getElementById('btn-limit-onoff');
  if (!btn) return;
  btn.textContent = S.limits.on ? t('on') : t('off');
  btn.classList.toggle('active', S.limits.on);
  btn.setAttribute('aria-pressed', String(S.limits.on));
}

/** Rebuild the editable point rows (called on init, edits, add/remove and language change). */
function renderRows(): void {
  const host = document.getElementById('limit-rows');
  if (!host) return;
  host.innerHTML = '';
  S.limits.points.forEach((p, i) => {
    const row = document.createElement('div');
    row.className = 'param-row';
    row.innerHTML =
      `<label>P${i + 1}</label>` +
      `<input type="number" class="limit-freq" step="any" value="${fmtMHz(p.freqHz)}"` +
      ` data-i18n="tip_limit_freq" data-i18n-attr="title">` +
      `<span class="cur-val">MHz</span>` +
      `<input type="number" class="limit-level" step="0.5" value="${toDisplayLevel(p.level).toFixed(1)}"` +
      ` data-i18n="tip_limit_level" data-i18n-attr="title">` +
      `<span class="cur-val">${S.levelUnit}</span>` +
      `<button class="btn limit-del" data-i="${i}" data-i18n="limit_del" data-i18n-title="tip_limit_del">\u00d7</button>`;
    host.appendChild(row);
  });
  applyI18n(host);
  host.querySelectorAll<HTMLInputElement>('.limit-freq').forEach((el, i) => {
    el.addEventListener('change', () => {
      const v = parseFloat(el.value);
      if (Number.isFinite(v)) S.limits.points[i].freqHz = v * 1e6;
      save();
      refreshLimitVerdict();
    });
  });
  host.querySelectorAll<HTMLInputElement>('.limit-level').forEach((el, i) => {
    el.addEventListener('change', () => {
      const v = parseFloat(el.value);
      if (Number.isFinite(v)) S.limits.points[i].level = fromDisplayLevel(v);
      save();
      refreshLimitVerdict();
    });
  });
  host.querySelectorAll<HTMLButtonElement>('.limit-del').forEach((el) => {
    el.addEventListener('click', () => {
      if (S.limits.points.length <= 2) return;   // a limit needs at least two points
      S.limits.points.splice(Number(el.dataset.i), 1);
      renderRows();
      save();
      refreshLimitVerdict();
    });
  });
}

/**
 * The verdict is shown on the canvas (render/statusStack.ts): the render pass evaluates the active
 * trace and pushes a LIMIT PASS / LIMIT FAIL block into the shared status area, so an edit only has
 * to ask for a repaint instead of updating a panel row.
 */
export function refreshLimitVerdict(): void {
  renderAll();
}

/** Re-render the point rows after a unit change (levels stay stored in dBm). */
export function refreshLimitUnits(): void {
  renderRows();
  refreshLimitVerdict();
}

export function exportLimitCsv(): void {
  const powers = getDisplayPowers();
  const freq = S.freqArray;
  if (!powers || !freq) return;
  const n = Math.min(powers.length, freq.length);
  const arr = buildLimitArray(freq, S.limits.points, n);
  const head = [
    '# limit_violations',
    `tol_db=${S.limits.tol}`,
    `points=${S.limits.points.map((p) => `${fmtMHz(p.freqHz)}MHz:${p.level.toFixed(2)}dBm`).join(' ')}`,
    `center_hz=${S.centerHz}`, `span_hz=${S.spanHz}`,
    `rbw_hz=${S.currentRBW}`, `points_display=${n}`,
    `time=${new Date().toISOString()}`,
    'freq_hz,level,limit,margin',
  ];
  const rows: string[] = [];
  if (arr) {
    for (let i = 0; i < n; i++) {
      const v = powers[i];
      const lim = arr[i];
      if (!Number.isFinite(v) || !Number.isFinite(lim)) continue;
      const margin = v - lim;
      if (margin > S.limits.tol) {
        rows.push(`${freq[i].toFixed(3)},${v.toFixed(3)},${lim.toFixed(3)},${margin.toFixed(3)}`);
      }
    }
  }
  const blob = new Blob([head.concat(rows).join('\n')], { type: 'text/csv;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `websa_limits_${new Date().toISOString().replace(/[:.]/g, '-')}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

/** Insert a point in the middle of the widest gap at the interpolated level. */
function addPoint(): void {
  const pts = normalizePoints(S.limits.points);
  if (pts.length < 2) {
    S.setLimits({ ...S.limits, points: spanLimitPoints() });
  } else {
    let gapIdx = 1;
    let gap = -1;
    for (let i = 1; i < pts.length; i++) {
      const w = pts[i].freqHz - pts[i - 1].freqHz;
      if (w > gap) { gap = w; gapIdx = i; }
    }
    const a = pts[gapIdx - 1];
    const b = pts[gapIdx];
    S.limits.points = [...pts, { freqHz: (a.freqHz + b.freqHz) / 2, level: (a.level + b.level) / 2 }]
      .sort((x, y) => x.freqHz - y.freqHz);
  }
  renderRows();
  save();
  refreshLimitVerdict();
}

export function initLimits(): void {
  load();
  renderRows();
  syncToggle();

  document.getElementById('btn-limit-onoff')?.addEventListener('click', () => {
    S.limits.on = !S.limits.on;
    if (S.limits.on && !normalizePoints(S.limits.points).length) S.limits.points = spanLimitPoints();
    syncToggle();
    save();
    refreshLimitVerdict();
  });
  document.getElementById('btn-limit-add')?.addEventListener('click', addPoint);
  document.getElementById('btn-limit-reset')?.addEventListener('click', () => {
    S.limits.points = spanLimitPoints();
    renderRows();
    save();
    refreshLimitVerdict();
  });
  document.getElementById('btn-limit-csv')?.addEventListener('click', exportLimitCsv);

  const tol = document.getElementById('input-limit-tol') as HTMLInputElement | null;
  if (tol) {
    tol.value = String(S.limits.tol);
    tol.addEventListener('change', () => {
      const v = parseFloat(tol.value);
      S.limits.tol = Number.isFinite(v) && v >= 0 ? v : 0;
      tol.value = String(S.limits.tol);
      save();
      refreshLimitVerdict();
    });
  }

  onLangChange(() => {
    syncToggle();
    renderRows();
    refreshLimitVerdict();
  });

  refreshLimitVerdict();
}
