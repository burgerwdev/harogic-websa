// 入口: 初始化所有模块
import './style.css';
import * as S from './core/store';
import { buildUnitGroups } from './core/units';
import { connectWS } from './core/ws';
import { initMarkerTable } from './render/markerTable';
import { renderAll } from './render/spectrum';
import { bindActions, bindCanvas, syncToggleIcons } from './ui/controls';
import { applyI18n, setLang, onLangChange, t } from './core/i18n';
import { initTheme, setTheme, onThemeChange, toggleTheme } from './core/theme';
import { updateInfoBar } from './render/infobar';
import { syncToggleTexts } from './ui/controls';

function init() {
  // 主题/语言(从 localStorage 恢复)
  const savedTheme = localStorage.getItem('web-sa-theme');
  initTheme(savedTheme === 'light' ? 'light' : 'dark');
  const savedLang = localStorage.getItem('web-sa-lang');
  setLang(savedLang === 'zh' ? 'zh' : 'en');
  applyI18n();

  buildUnitGroups();
  initMarkerTable();
  syncToggleIcons();
  bindActions();
  bindCanvas();

  // 主题/语言切换按钮(dev-bar, preset 旁, 无 label)
  const btnTheme = document.getElementById('btn-theme');
  if (btnTheme) {
    btnTheme.textContent = S_getTheme() === 'dark' ? 'Light' : 'Dark';
    btnTheme.addEventListener('click', () => {
      const th = toggleTheme();
      localStorage.setItem('web-sa-theme', th);
      btnTheme.textContent = th === 'dark' ? 'Light' : 'Dark';
      renderAll();
    });
  }
  const btnLang = document.getElementById('btn-lang');
  if (btnLang) {
    btnLang.textContent = getLang() === 'en' ? '中文' : 'EN';
    btnLang.addEventListener('click', () => {
      const l = getLang() === 'en' ? 'zh' : 'en';
      setLang(l);
      localStorage.setItem('web-sa-lang', l);
      applyI18n();
      updateInfoBar();
      syncToggleTexts();
      btnLang.textContent = l === 'en' ? '中文' : 'EN';
      renderAll();
    });
  }
  onThemeChange(() => { renderAll(); });

  // WS + 周期渲染(测量视图)
  connectWS();
  setInterval(() => { if (S.viewMode === 'pnm' || S.viewMode === 'harm') renderAll(); }, 200);
}

import { getTheme as S_getTheme, getTheme } from './core/theme';
import { getLang } from './core/i18n';

init();
