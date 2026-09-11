// Marker table (fixed layout)
import * as S from '../core/store';
import { fmtLevel } from '../core/level';
import { formatFreqHz } from '../core/fmt';
import { markerFreqHz } from '../core/markerCommon';
import { renderAll } from './spectrum';
import { t } from '../core/i18n';
import { assignMarkerToBestPeak } from '../dsp/markerTracking';

export function initMarkerTable() {
  const tbody = document.getElementById('marker-tbody');
  if (!tbody || tbody.children.length) return;
  S.markers.forEach((m, mi) => {
    const tr = document.createElement('tr');
    tr.dataset.mid = String(m.id);

    const td0 = document.createElement('td');
    td0.className = 'mk-cell';
    // Active-marker indicator: click it to make this the active marker (the same as the
    // right-hand Marker buttons). The M toggle next to it still switches on/off.
    const activeBtn = document.createElement('button');
    activeBtn.className = 'mk-active';
    activeBtn.type = 'button';
    activeBtn.title = 'Set as active marker';
    activeBtn.textContent = '\u25b6';
    activeBtn.style.setProperty('--marker-color', `var(--m${mi + 1})`);
    activeBtn.onclick = () => document.dispatchEvent(
      new CustomEvent('websa:marker-active', { detail: { id: m.id } }));
    td0.appendChild(activeBtn);
    const toggle = document.createElement('button');
    toggle.className = 'marker-toggle';
    toggle.type = 'button';
    toggle.style.setProperty('--marker-color', `var(--m${mi + 1})`);
    toggle.setAttribute('aria-pressed', 'false');
    toggle.onclick = () => toggleMarkerEnabled(m.id);
    const swatch = document.createElement('span');
    swatch.className = 'mk-swatch';
    const label = document.createElement('span');
    label.textContent = `M${m.id}`;
    const tracking = document.createElement('span');
    tracking.className = 'mk-track-indicator';
    tracking.textContent = 'T';
    toggle.append(swatch, label, tracking);
    td0.appendChild(toggle);
    tr.appendChild(td0);

    const td1 = document.createElement('td');
    const sel = document.createElement('select');
    // OFF is not offered: the M toggle already switches the marker on/off.
    ['NORMAL', 'DELTA'].forEach(o => {
      const op = document.createElement('option'); op.value = o; op.textContent = o; sel.appendChild(op);
    });
    sel.onchange = () => updateMarkerMode(m.id, sel.value);
    td1.appendChild(sel); tr.appendChild(td1);

    const td2 = document.createElement('td'); td2.className = 'num'; td2.textContent = '-'; tr.appendChild(td2);
    const td3 = document.createElement('td'); td3.className = 'num'; td3.textContent = '-'; tr.appendChild(td3);
    const td4 = document.createElement('td');
    const selr = document.createElement('select');
    for (let i = 0; i < 4; i++) {
      const op = document.createElement('option'); op.value = String(i + 1); op.style.color = S.MARKER_COLORS[i]; op.textContent = `M${i + 1}`; selr.appendChild(op);
    }
    selr.value = String(m.refId);
    selr.onchange = () => {
      m.refId = parseInt(selr.value);
      if (m.mode !== 'OFF') m.mode = 'DELTA';
      renderAll();
    };
    td4.appendChild(selr); tr.appendChild(td4);

    tbody.appendChild(tr);
  });
}

export function updateMarkerTable(powers: Float32Array | null) {
  initMarkerTable();
  const tbody = document.getElementById('marker-tbody');
  if (!tbody) return;
  const unit = S.displayUnit === 'dB' ? ' dB' : null;
  S.markers.forEach((m, i) => {
    const row = tbody.children[i] as HTMLTableRowElement;
    if (!row) return;
    const cells = row.children;
    const markerToggle = cells[0].querySelector('.marker-toggle') as HTMLButtonElement;
    const activeBtn = cells[0].querySelector('.mk-active') as HTMLButtonElement | null;
    const anyOn = S.markers.some(mk => mk.enabled && mk.mode !== 'OFF');
    if (activeBtn) activeBtn.classList.toggle('active', anyOn && m.id === S.activeMkrId);
    document.querySelectorAll('.mkr-btn').forEach((btn) => {
      const id = Number((btn as HTMLElement).getAttribute('data-marker-select'));
      btn.classList.toggle('active', anyOn && id === S.activeMkrId);
    });
    const markerOn = m.enabled && m.mode !== 'OFF';
    markerToggle.classList.toggle('active', markerOn);
    markerToggle.classList.toggle('tracking', markerOn && m.tracking);
    markerToggle.setAttribute('aria-pressed', String(markerOn));
    markerToggle.title = markerOn ? t('off') : t('on');
    const selMode = cells[1].querySelector('select') as HTMLSelectElement;
    const showMode = m.mode === 'OFF' ? 'NORMAL' : m.mode;
    if (document.activeElement !== selMode && selMode.value !== showMode) selMode.value = showMode;
    selMode.disabled = !markerOn;
    const selRef = cells[4].querySelector('select') as HTMLSelectElement;
    if (document.activeElement !== selRef && selRef.value !== String(m.refId)) selRef.value = String(m.refId);
    selRef.disabled = !markerOn;

    let fStr = '-', aStr = '-';
    if (m.enabled && m.mode !== 'OFF' && powers) {
      const idx = Math.min(m.idx, powers.length - 1);
      const f = markerFreqHz(idx), a = powers[idx];
      if (m.mode === 'NORMAL') { fStr = formatFreqHz(f); aStr = unit ? a.toFixed(2) + unit : fmtLevel(a); }
      else if (m.mode === 'DELTA') {
        const ref = S.markers.find(x => x.id === m.refId);
        if (ref) {
          const rf = markerFreqHz(Math.min(ref.idx, powers.length - 1));
          const ra = powers[Math.min(ref.idx, powers.length - 1)];
          fStr = 'Δ' + formatFreqHz(Math.abs(f - rf)) + ' / ' + formatFreqHz(f);
          aStr = `Δ${(a - ra).toFixed(2)} dBc / ${unit ? a.toFixed(2) + unit : fmtLevel(a)}`;
        }
      }
    }
    if (cells[2].textContent !== fStr) cells[2].textContent = fStr;
    if (cells[3].textContent !== aStr) cells[3].textContent = aStr;
  });
  const active = S.markers.find(marker => marker.id === S.activeMkrId);
  const trackingButton = document.getElementById('btn-marker-tracking');
  if (active && trackingButton) {
    trackingButton.textContent = t('tracking');
    trackingButton.classList.toggle('active', active.tracking);
    trackingButton.setAttribute('aria-pressed', String(active.tracking));
  }
}

function toggleMarkerEnabled(id: number) {
  const marker = S.markers.find(item => item.id === id);
  if (!marker) return;
  marker.enabled = !marker.enabled;
  if (marker.enabled && marker.mode === 'OFF') marker.mode = 'NORMAL';
  if (marker.enabled) assignMarkerToBestPeak(marker);
  renderAll();
}

function updateMarkerMode(id: number, mode: string) {
  const m = S.markers.find(x => x.id === id);
  if (!m) return;
  if (mode === 'OFF') {
    m.enabled = false;
    m.mode = 'OFF';
    m.tracking = false;
  }
  else { m.enabled = true; m.mode = mode; }
  renderAll();
}
