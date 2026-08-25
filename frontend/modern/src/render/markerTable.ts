// Marker table (fixed layout)
import * as S from '../core/store';
import { formatFreqHz } from '../core/fmt';
import { markerFreqHz } from '../core/markerCommon';
import { renderAll } from './spectrum';

export function initMarkerTable() {
  const tbody = document.getElementById('marker-tbody');
  if (!tbody || tbody.children.length) return;
  S.markers.forEach((m, mi) => {
    const tr = document.createElement('tr');
    tr.dataset.mid = String(m.id);

    const td0 = document.createElement('td');
    td0.innerHTML = `<span class="mk-cell"><span class="mk-swatch" style="background:var(--m${mi + 1})"></span>M${m.id}</span>`;
    tr.appendChild(td0);

    const td1 = document.createElement('td');
    const sel = document.createElement('select');
    ['OFF', 'NORMAL', 'DELTA'].forEach(o => {
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
  const unit = S.displayUnit === 'dB' ? ' dB' : ' dBm';
  S.markers.forEach((m, i) => {
    const row = tbody.children[i] as HTMLTableRowElement;
    if (!row) return;
    const cells = row.children;
    (cells[0] as HTMLElement).style.opacity = (m.enabled && m.mode !== 'OFF') ? '1' : '0.4';
    const selMode = cells[1].querySelector('select') as HTMLSelectElement;
    if (document.activeElement !== selMode && selMode.value !== m.mode) selMode.value = m.mode;
    const selRef = cells[4].querySelector('select') as HTMLSelectElement;
    if (document.activeElement !== selRef && selRef.value !== String(m.refId)) selRef.value = String(m.refId);

    let fStr = '-', aStr = '-';
    if (m.enabled && m.mode !== 'OFF' && powers) {
      const idx = Math.min(m.idx, powers.length - 1);
      const f = markerFreqHz(idx), a = powers[idx];
      if (m.mode === 'NORMAL') { fStr = formatFreqHz(f); aStr = a.toFixed(2) + unit; }
      else if (m.mode === 'DELTA') {
        const ref = S.markers.find(x => x.id === m.refId);
        if (ref) {
          const rf = markerFreqHz(Math.min(ref.idx, powers.length - 1));
          const ra = powers[Math.min(ref.idx, powers.length - 1)];
          fStr = 'Δ' + formatFreqHz(Math.abs(f - rf)) + ' / ' + formatFreqHz(f);
          aStr = `Δ${(a - ra).toFixed(2)} dBc / ${a.toFixed(2)}${unit}`;
        }
      }
    }
    if (cells[2].textContent !== fStr) cells[2].textContent = fStr;
    if (cells[3].textContent !== aStr) cells[3].textContent = aStr;
  });
}

function updateMarkerMode(id: number, mode: string) {
  const m = S.markers.find(x => x.id === id);
  if (!m) return;
  if (mode === 'OFF') { m.enabled = false; m.mode = 'OFF'; }
  else { m.enabled = true; m.mode = mode; }
  renderAll();
}
