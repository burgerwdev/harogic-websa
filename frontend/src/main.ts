// SPDX-License-Identifier: LicenseRef-Harogic-WebSA
// Project-owned code: AGPL-3.0-only with ../../LICENSE-EXCEPTION; see ../../LICENSING.md.
// Entry point: initialize all modules
import './style.css';
import { initSdrScan } from './ui/sdrScan';
import * as S from './core/store';
import { buildUnitGroups } from './core/units';
import { connectWS } from './core/ws';
import { initMarkerTable } from './render/markerTable';
import { initLimits, refreshLimitUnits } from './ui/limits';
import { initControlRail } from './ui/controlRail';
import { initTrigger } from './ui/trigger';
import { initWfRange } from './ui/wfRange';
import { initKeypad } from './ui/keypad';
import { initPopoverDrag } from './ui/popoverDrag';
import { initLevelUnit } from './core/level';
import { updateChanTable } from './meas/channel';
import { requestRender, renderStatus } from './render/redraw';
import { initStore } from './core/store';
import { initUiScale } from './core/uiScale';
// Imported for its registration side effects: render/spectrum.ts is the swept/RTA view hub
// and calls setRenderer(renderAll) + registerViewRenderer('rta') at module scope. Nothing
// imports it any more (that is what broke the render/spectrum cycles in the first place), so
// the entry point must pull it in explicitly - otherwise requestRender() has no renderer and
// the canvas stays blank.
import './render/spectrum';
import { bindActions, bindCanvas, syncToggleIcons } from './ui/controls';
import { initSpectrumZoomUi } from './ui/spectrumZoomUi';
import { initSdrEqualizer } from './ui/sdrEqualizer';
import { applyI18n, setLang } from './core/i18n';
import { initTheme, onThemeChange } from './core/theme';
import { initThemePicker } from './ui/themePicker';
import { initLiquidGlass } from './ui/liquidGlass';
import { initWindowOpacity } from './ui/windowOpacity';
import { updateInfoBar } from './render/infobar';
import { syncToggleTexts } from './ui/controls';

function init() {
  Object.assign(window, { websaRenderStatus: renderStatus });
  // The UI scale first: the frame is CSS-zoomed, so it decides the box the canvas gets when
  // initStore() sizes it (core/uiScale.ts, core/store.ts).
  initUiScale();
  initStore();   // bind the canvas before the first render
  // Theme/language (restored from localStorage)
  let savedTheme: string | null = null, savedLang: string | null = null;
  try {
    savedTheme = localStorage.getItem('web-sa-theme');
    savedLang = localStorage.getItem('web-sa-lang');
  } catch { /* Optional preferences must not prevent controls from starting. */ }
  initTheme(savedTheme);
  initWindowOpacity();
  setLang(savedLang === 'zh' ? 'zh' : 'en');
  applyI18n();

  buildUnitGroups();
  initMarkerTable();
  initLimits();
  initControlRail();
  initTrigger();
  initWfRange();
  initKeypad();
  initLevelUnit(() => { refreshLimitUnits(); updateChanTable(); requestRender(); });
  syncToggleIcons();
  bindActions();
  bindCanvas();
  initSdrScan();
  initSpectrumZoomUi();
  initSdrEqualizer();
  initPopoverDrag();
  // Autoplay can hold playback silent after a reload; repaint when that state
  // changes so the status hint appears and clears without another trigger.
  document.addEventListener('websa:sdr-audio-gesture', () => requestRender());


  initThemePicker();
  initLiquidGlass();
  // Language button (dev-bar, next to appearance).
  const btnLang = document.getElementById('btn-lang');
  if (btnLang) {
    btnLang.textContent = getLang() === 'en' ? 'EN' : '中文';
    btnLang.addEventListener('click', () => {
      const l = getLang() === 'en' ? 'zh' : 'en';
      setLang(l);
      try { localStorage.setItem('web-sa-lang', l); } catch { /* session only */ }
      applyI18n();
      updateInfoBar();
      syncToggleTexts();
      btnLang.textContent = l === 'en' ? 'EN' : '中文';
      requestRender();
    });
  }
  onThemeChange(() => { requestRender(); });

  // WS + periodic render (measurement view)
  connectWS();
  setInterval(() => { if (!document.hidden && (S.viewMode === 'pnm' || S.viewMode === 'harm')) requestRender(); }, 200);
}

import { getLang } from './core/i18n';
import { sampleEnvelope, sampleSpectrum } from './audio/sdrAudio';
import { renderRequestCount } from './render/redraw';

init();

// Console diagnostics boundary: window globals are the one channel a bug reporter can reach
// without a rebuild, so they are declared once here instead of cast at every assignment.
const diagnostics = window as unknown as {
	websaSpectrum: typeof sampleSpectrum;
	websaEnvelope: typeof sampleEnvelope;
	websaRenderRequests: () => number;
};
diagnostics.websaSpectrum = sampleSpectrum;
diagnostics.websaEnvelope = sampleEnvelope;
// Repaint diagnostics expose request frequency and triggers without adding work to the render path.
diagnostics.websaRenderRequests = renderRequestCount;
