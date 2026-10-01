// i18n: the en/zh dictionary, merged from per-domain namespaces (core/trigger/sdr/limits/
// keypad) so a feature's text ships with its feature (report finding E-5).
//
// History: this file used to hold a base literal plus thirteen additive Object.assign blocks,
// which left `keyof typeof dict.en` covering only the base literal and made key parity
// impossible to check; it was merged into one declaration (2026-09) and later split by
// namespace while keeping a single merged `dict` for t()/hasKey and the parity test.
import * as dict_core from './i18n/dict.core';
import * as dict_trigger from './i18n/dict.trigger';
import * as dict_sdr from './i18n/dict.sdr';
import * as dict_limits from './i18n/dict.limits';
import * as dict_keypad from './i18n/dict.keypad';

const dict = {
  en: { ...dict_core.en, ...dict_trigger.en, ...dict_sdr.en, ...dict_limits.en, ...dict_keypad.en },
  zh: { ...dict_core.zh, ...dict_trigger.zh, ...dict_sdr.zh, ...dict_limits.zh, ...dict_keypad.zh },
};

export type Lang = 'en' | 'zh';

export type I18nKey = keyof typeof dict['en'];

/** The raw dictionaries (used by the integrity tests in `__tests__/i18n.test.ts`). */
export { dict };

let current: Lang = 'en';
const listeners = new Set<(l: Lang) => void>();

export function t(key: string, params?: Record<string, string | number>): string {
  const en = dict.en as Record<string, string>;
  const raw = current === 'zh'
    ? ((dict.zh as Record<string, string>)[key] ?? en[key] ?? key)
    : (en[key] ?? key);
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (_, name: string) =>
    params[name] !== undefined ? String(params[name]) : `{${name}}`);
}

/** True when the dictionary defines a key (t() falls back to the key text itself). */
export function hasKey(key: string): boolean {
  const en = dict.en as Record<string, string>;
  return key in en || (dict.zh as Record<string, string>)[key] !== undefined;
}

export function getLang(): Lang { return current; }

export function setLang(l: Lang) {
  current = l;
  document.documentElement.lang = l === 'zh' ? 'zh-CN' : 'en';
  listeners.forEach((fn) => fn(l));
}

export function onLangChange(fn: (l: Lang) => void) {
  listeners.add(fn);
}

// Render all data-i18n nodes (text or placeholder/title)
export function applyI18n(root: HTMLElement | Document = document) {
  root.querySelectorAll<HTMLElement>('[data-i18n]').forEach((el) => {
    const k = el.dataset.i18n!;
    if (el.dataset.i18nAttr === 'placeholder') el.setAttribute('placeholder', t(k));
    else {
      // Tooltip: data-i18n-title overrides the label key; data-i18n-attr="title" marks
      // title-only elements (glyph buttons keep their symbol as text).
      if (el.dataset.i18nTitle) el.title = t(el.dataset.i18nTitle);
      else if (el.dataset.i18nAttr === 'title') el.title = t(k);
      if (el.dataset.i18nAttr !== 'title') el.textContent = t(k);
    }
  });
  // Labels that are computed (not plain dictionary entries) must also follow the language.
  // The theme button shows the *target* theme, which depends on the active theme.
  const themeBtn = document.getElementById('btn-theme');
  if (themeBtn) {
    themeBtn.textContent = document.documentElement.dataset.theme === 'dark' ? t('dark') : t('light');
  }
}
