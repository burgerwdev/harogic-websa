// Entry point: initialize all modules
import './style.css';
import * as S from './core/store';
import { buildUnitGroups } from './core/units';
import { connectWS } from './core/ws';
import { initMarkerTable } from './render/markerTable';
import { initLimits, refreshLimitUnits } from './ui/limits';
import { initControlRail } from './ui/controlRail';
import { initTrigger } from './ui/trigger';
import { initLevelUnit } from './core/level';
import { updateChanTable } from './meas/channel';
import { renderAll } from './render/spectrum';
import { bindActions, bindCanvas, syncToggleIcons } from './ui/controls';
import { applyI18n, setLang, onLangChange, t } from './core/i18n';
import { initTheme, setTheme, onThemeChange, toggleTheme } from './core/theme';
import { updateInfoBar } from './render/infobar';
import { syncToggleTexts } from './ui/controls';

function init() {
  // Theme/language (restored from localStorage)
  const savedTheme = localStorage.getItem('web-sa-theme');
  initTheme(savedTheme === 'light' ? 'light' : 'dark');
  const savedLang = localStorage.getItem('web-sa-lang');
  setLang(savedLang === 'zh' ? 'zh' : 'en');
  applyI18n();

  buildUnitGroups();
  initMarkerTable();
  initLimits();
  initControlRail();
  initTrigger();
  initLevelUnit(() => { refreshLimitUnits(); updateChanTable(); renderAll(); });
  syncToggleIcons();
  bindActions();
  bindCanvas();

  // Restore saved mode (non-first load keeps RTA; first load defaults to std)
  const savedMode = localStorage.getItem('web-sa-mode');
  if (savedMode === 'rta') {
    S.setViewMode('rta');
    S.setRtaMode(true);
    const bRta = document.getElementById('btn-mode-rta');
    if (bRta) bRta.classList.add('active');
    const rtaF = document.getElementById('rta-freq-settings');
    const swpF = document.getElementById('swp-freq-settings');
    if (rtaF) rtaF.style.display = '';
    if (swpF) swpF.style.display = 'none';
  }

  // Theme/language toggle buttons (dev-bar, next to preset, no label)
  const btnTheme = document.getElementById('btn-theme');
  if (btnTheme) {
    btnTheme.textContent = S_getTheme() === 'dark' ? t('dark') : t('light');
    btnTheme.addEventListener('click', () => {
      const th = toggleTheme();
      localStorage.setItem('web-sa-theme', th);
      btnTheme.textContent = th === 'dark' ? t('dark') : t('light');
      renderAll();
    });
  }
  const btnLang = document.getElementById('btn-lang');
  if (btnLang) {
    btnLang.textContent = getLang() === 'en' ? 'EN' : '中文';
    btnLang.addEventListener('click', () => {
      const l = getLang() === 'en' ? 'zh' : 'en';
      setLang(l);
      localStorage.setItem('web-sa-lang', l);
      applyI18n();
      updateInfoBar();
      syncToggleTexts();
      btnLang.textContent = l === 'en' ? 'EN' : '中文';
      renderAll();
    });
  }
  onThemeChange(() => { renderAll(); });

  // WS + periodic render (measurement view)
  connectWS();
  setInterval(() => { if (S.viewMode === 'pnm' || S.viewMode === 'harm') renderAll(); }, 200);
}

import { getTheme as S_getTheme, getTheme } from './core/theme';
import { getLang } from './core/i18n';

init();
