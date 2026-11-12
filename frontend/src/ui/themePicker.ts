import { getTheme, isTheme, onThemeChange, setTheme, THEMES } from '../core/theme';
import { applyI18n, onLangChange, t } from '../core/i18n';
import './themePicker.css';

/** A native dialog keeps focus and Escape behavior consistent, including at large UI scales. */
export function initThemePicker(): void {
  const trigger = document.getElementById('btn-theme');
  if (!trigger) return;
  const dialog = document.createElement('dialog');
  dialog.id = 'theme-picker';
  dialog.className = 'theme-picker';
  dialog.setAttribute('aria-labelledby', 'theme-picker-title');
  dialog.setAttribute('aria-describedby', 'theme-picker-hint');
  dialog.innerHTML = `
    <div class="theme-picker-head">
      <div><h2 id="theme-picker-title" data-i18n="theme_picker_title"></h2>
      <p id="theme-picker-hint" data-i18n="theme_picker_hint"></p></div>
      <button type="button" class="theme-picker-close" data-i18n="theme_picker_close" data-i18n-attr="title">
        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="m6 6 12 12M18 6 6 18"/></svg>
      </button>
    </div>
    <div class="theme-picker-grid" role="group" aria-labelledby="theme-picker-title">
      ${THEMES.map(theme => `
        <button type="button" class="theme-option" data-theme-choice="${theme.id}" aria-pressed="false">
          <span class="theme-sample" data-theme-preview="${theme.id}" aria-hidden="true">
            <span class="theme-sample-bar"><i></i><i></i><i></i></span>
            <span class="theme-sample-body">
              <svg viewBox="0 0 120 48" preserveAspectRatio="none">
                <path class="theme-sample-grid" d="M0 12H120M0 24H120M0 36H120M24 0V48M48 0V48M72 0V48M96 0V48"/>
                <path class="theme-sample-trace" d="M0 39 9 38 12 40 19 37 23 15 26 38 34 39 41 36 48 38 54 34 59 7 64 37 72 38 78 36 83 21 87 38 98 37 104 39 113 36 120 38"/>
              </svg>
              <span class="theme-sample-controls"><i></i><i></i><i></i></span>
            </span>
          </span>
          <span class="theme-option-label"><span data-i18n="${theme.name}"></span><span class="theme-option-check" aria-hidden="true">✓</span></span>
        </button>`).join('')}
    </div>
    <p class="theme-picker-note" data-i18n="theme_picker_note"></p>`;
  document.body.appendChild(dialog);
  trigger.setAttribute('aria-controls', dialog.id);
  trigger.setAttribute('aria-haspopup', 'dialog');
  trigger.setAttribute('aria-expanded', 'false');
  const options = Array.from(dialog.querySelectorAll<HTMLButtonElement>('[data-theme-choice]'));
  const sync = () => {
    const active = THEMES.find(theme => theme.id === getTheme())!;
    trigger.textContent = t(active.name);
    trigger.title = t('tip_theme');
    for (const option of options) {
      const selected = option.dataset.themeChoice === active.id;
      option.setAttribute('aria-pressed', String(selected));
    }
    applyI18n(dialog);
    dialog.querySelector('.theme-picker-close')!.setAttribute('aria-label', t('theme_picker_close'));
  };
  const close = () => { dialog.close(); trigger.setAttribute('aria-expanded', 'false'); };
  trigger.addEventListener('click', () => {
    sync();
    dialog.showModal();
    trigger.setAttribute('aria-expanded', 'true');
    options.find(option => option.dataset.themeChoice === getTheme())?.focus();
  });
  dialog.addEventListener('close', () => trigger.setAttribute('aria-expanded', 'false'));
  dialog.querySelector('.theme-picker-close')!.addEventListener('click', close);
  // Only a press and release on the backdrop dismisses the dialog; dragging out of a card does not.
  let backdropPress = false;
  dialog.addEventListener('pointerdown', event => {
    const rect = dialog.getBoundingClientRect();
    backdropPress = event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right
      || event.clientY < rect.top || event.clientY > rect.bottom);
  });
  dialog.addEventListener('click', event => {
    if (event.target === dialog && backdropPress) close();
    backdropPress = false;
  });
  for (const option of options) option.addEventListener('click', () => {
    const theme = option.dataset.themeChoice;
    if (!isTheme(theme)) return;
    setTheme(theme);
    try { localStorage.setItem('web-sa-theme', theme); } catch { /* session only */ }
    close();
  });
  dialog.addEventListener('keydown', event => {
    const index = options.indexOf(document.activeElement as HTMLButtonElement);
    if (index < 0) return;
    const step = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: 3, ArrowUp: -3 }[event.key];
    let next: number;
    if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = options.length - 1;
    else if (step !== undefined) next = (index + step + options.length) % options.length;
    else return;
    event.preventDefault();
    options[next].focus();
  });
  onThemeChange(sync);
  onLangChange(sync);
  sync();
}
