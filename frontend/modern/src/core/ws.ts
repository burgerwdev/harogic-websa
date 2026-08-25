// WebSocket protocol layer + STATUS handling
import * as S from './store';
import { formatBWHz, formatFreqHz, fmtAxis } from './fmt';
import { toUnit } from './units';
import { t } from './i18n';
import { updateInfoBar } from '../render/infobar';
import { syncRefClkOut, fillGnssDetail } from '../ui/controls';
import { invalidateAllTraces } from '../dsp/traces';
import { setWS } from './wsSend';
import { retrackMarkers } from './markerCommon';
import { processTraces } from '../dsp/traces';
import { renderAll } from '../render/spectrum';
import { onHarmResult } from '../meas/harmonic';
import { onPnmResult } from '../meas/phaseNoise';
import { updateNormalizeStatusUI } from '../dsp/normalize';

let ws: WebSocket;
let lastRender = 0;

export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

export function connectWS() {
  ws = new WebSocket(`ws://${location.host}/ws`);
  setWS(ws);   // Key: all commands (send) go through the unified wsSend exit, must be initialized
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => { send({ cmd: 'STATUS' }); };
  ws.onclose = () => {
    S.setDeviceConnected(false);
    updateInfoBar();
    setTimeout(connectWS, 2000);
  };
  ws.onmessage = (event: MessageEvent) => {
    if (typeof event.data === 'string') {
      const msg = JSON.parse(event.data as string);
      if (msg.cmd === 'STATUS') updateStatus(msg);
      else if (msg.cmd === 'HARM') onHarmResult(msg.list);
      else if (msg.cmd === 'PNM') onPnmResult(msg);
      else if (msg.cmd === 'ERROR') alert('Device: ' + msg.msg);
      return;
    }
    const view = new DataView(event.data, 0, 16);
    const magic = String.fromCharCode(view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
    const version = view.getUint32(4, true);
    const points = view.getUint32(8, true);
    const sweepMsHdr = view.getFloat32(12, true);
    if (sweepMsHdr > 0 && sweepMsHdr !== S.sweepMs) { S.setSweepMs(sweepMsHdr); updateInfoBar(); }
    if (points < 2) return;
    if (magic === 'FREQ') {
      S.setFreqArray(new Float64Array(event.data, 16, points));
      S.setFreqVersion(version);
      const el = document.getElementById('info-pts');
      if (el) el.innerText = String(points);
      retrackMarkers();
    } else if (magic === 'POWR') {
      if (version !== S.freqVersion) return;
      const raw = new Float32Array(event.data, 16, points);
      processTraces(raw);
      const now = performance.now();
      if (now - lastRender >= 33) {
        lastRender = now;
        renderAll();
      }
    }
  };
}

export function updateStatus(s: any) {
  S.setCenterHz(s.actual.center > 0 ? s.actual.center : s.req.center);
  S.setSpanHz(s.actual.span > 0 ? s.actual.span : s.req.span);
  S.setRefLevel(s.actual.ref > 0 ? s.actual.ref : s.req.ref);
  S.setCurrentRBW(s.actual.rbw > 0 ? s.actual.rbw : s.req.rbw);
  S.setCurrentVBW(s.actual.vbw > 0 ? s.actual.vbw : s.req.vbw);
  S.setRbwMode(s.req.rbw_mode);
  S.setVbwMode(s.req.vbw_mode);
  S.setCurrentPoints(s.req.points || s.actual.points);
  S.setCurrentSpur(s.req.spur);
  S.setSweepMs(s.sweep_ms || 0);
  S.setDeviceConnected(!!s.connected);

  const measKey = `${S.centerHz}|${S.spanHz}|${S.currentPoints}|${S.currentRBW}|${S.rbwMode}|${s.window}`;
  if (measKey !== S.lastMeasKey) {
    S.setLastMeasKey(measKey);
    invalidateAllTraces();
  }
  if (S.displayUnit !== 'dB' && !S.refUserSet) S.setDisplayRef(S.refLevel);

  updateFreqUIInputs();
  setInput('input-ref', S.displayUnit === 'dB' ? '0' : S.displayRef.toFixed(0));
  setInput('input-points', String(S.currentPoints));
  setSelect('select-rbw-mode', S.rbwMode);
  setSelect('select-vbw-mode', S.vbwMode);
  setSelect('select-spur', S.currentSpur);
  const wsel = document.getElementById('select-window') as HTMLSelectElement;
  if (wsel && document.activeElement !== wsel && s.window != null) wsel.value = String(s.window);

  const dd = s.device_detail;
  if (dd) {
    const set = (id: string, v: string) => { const el = document.getElementById(id); if (el) el.textContent = v; };
    set('dev-model', 'Model ' + dd.model);
    set('dev-uid', dd.uid);
    set('dev-hw', dd.hw);
    set('dev-mfw', dd.mfw);
    set('dev-ffw', dd.ffw);
    set('dev-bus', dd.bus_speed + ' / v' + dd.bus_ver);
    set('dev-api', 'v' + dd.api_ver);
    set('dev-warn', dd.warnings);
  }
  S.setLastGnss(s.gnss);
  const g = document.getElementById('info-gnss');
  if (g) {
    // The top bar shows only the locked state (details such as satellite count live in the detail popover)
    if (s.gnss && s.gnss.lock) { g.textContent = t('status_locked'); g.style.color = 'var(--dot-on)'; }
    else { g.textContent = t('status_nolock'); g.style.color = 'var(--dot-off)'; }
    // Update the detail popover synchronously (if open)
    const pop = document.getElementById('gnss-popover');
    if (pop && pop.style.display !== 'none') fillGnssDetail();
  }
  const rc = document.getElementById('select-refclk') as HTMLSelectElement;
  syncRefClkOut(s);
  if (rc) {
    const hasOpt = rc.querySelector('option[value="premium"]');
    if (s.has_docxo && !hasOpt) {
      const o = document.createElement('option');
      o.value = 'premium'; o.textContent = 'Int+ (DOCXO)';
      rc.appendChild(o);
    } else if (!s.has_docxo && hasOpt) {
      hasOpt.remove();
    }
    if (document.activeElement !== rc) rc.value = s.ref_clock;
  }
  if (s.amp) {
    setSelect('select-atten', String(s.amp.atten));
    setSelect('select-preamp', String(s.amp.preamp));
    setSelect('select-ifgain', String(s.amp.ifgain));
    setSelect('select-gainstrategy', String(s.amp.gain_strategy));
    const ca = document.getElementById('cur-atten');
    if (ca) ca.textContent = s.amp.atten_actual >= 0 ? s.amp.atten_actual + ' dB' : '';
    const cp = document.getElementById('cur-preamp');
    if (cp) cp.textContent = s.amp.preamp_actual === 1 ? t('off') : (s.amp.preamp_actual === 0 ? t('on') : '');
    const cg = document.getElementById('cur-ifgain');
    if (cg) cg.textContent = s.amp.ifgain_actual != null ? 'L' + s.amp.ifgain_actual : '';
  }
  const of = document.getElementById('input-offset') as HTMLInputElement;
  if (of && document.activeElement !== of && Math.abs(parseFloat(of.value) - S.displayOffset) > 0.01)
    of.value = S.displayOffset.toFixed(1);
  const gf = document.getElementById('btn-gapfill');
  if (gf) {
    gf.textContent = S.currentGapFill ? t('on') : t('off');
    gf.classList.toggle('active', !!S.currentGapFill);
  }
  updateInfoBar();
}

function setInput(id: string, v: string) {
  const el = document.getElementById(id) as HTMLInputElement;
  if (el && document.activeElement !== el) el.value = v;
}
function setSelect(id: string, v: string) {
  const el = document.getElementById(id) as HTMLSelectElement;
  if (el && document.activeElement !== el) el.value = v;
}

// Sync frequency input fields
export function updateFreqUIInputs() {
  setInput('input-center', toUnit(S.centerHz, 'center').toFixed(4));
  setInput('input-span', toUnit(S.spanHz, 'span').toFixed(4));
  setInput('input-start', toUnit(S.centerHz - S.spanHz / 2, 'start').toFixed(4));
  setInput('input-stop', toUnit(S.centerHz + S.spanHz / 2, 'stop').toFixed(4));
  setInput('input-rbw', toUnit(S.currentRBW, 'rbw').toFixed(2));
  setInput('input-vbw', toUnit(S.currentVBW, 'vbw').toFixed(2));
}

// i18n sync: connect button/status text
export function syncConnectBtn() {
  const bc = document.getElementById('btn-connect');
  if (bc) bc.textContent = S.deviceConnected ? t('status_connected') : t('connect');
}
