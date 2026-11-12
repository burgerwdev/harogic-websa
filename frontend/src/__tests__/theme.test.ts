import { afterEach, expect, it, vi } from 'vitest';
import { canvasColors, getTheme, initTheme, onThemeChange, setTheme, THEMES } from '../core/theme';

afterEach(() => { vi.restoreAllMocks(); setTheme('dark'); });

it.each(THEMES)('restores the saved $id theme without changing its persisted ID', ({ id }) => {
  expect(initTheme(id)).toBe(id);
  expect(getTheme()).toBe(id);
  expect(document.documentElement.dataset.theme).toBe(id);
});

it.each([undefined, null, '', 'removed-theme', '__proto__', {}, 1])('defaults safely for an invalid saved preference: %s', saved => {
  expect(initTheme(saved)).toBe('dark');
  expect(document.documentElement.dataset.theme).toBe('dark');
});

it('reuses canvas colours through repeated draws and invalidates before theme listeners repaint', () => {
  const style = vi.spyOn(window, 'getComputedStyle').mockImplementation(() => ({
    getPropertyValue: (name: string) => name === '--sa-bg'
      ? document.documentElement.dataset.theme === 'light' ? '#ffffff' : '#000000' : '',
  }) as CSSStyleDeclaration);
  setTheme('dark');
  const dark = canvasColors();
  for (let i = 0; i < 100; i++) expect(canvasColors()).toBe(dark);
  expect(style).toHaveBeenCalledTimes(1);
  let repainted = '';
  const stop = onThemeChange(() => { repainted = canvasColors().bg; });
  try {
    setTheme('light');
    expect(repainted).toBe('#ffffff');
    expect(canvasColors()).not.toBe(dark);
    expect(style).toHaveBeenCalledTimes(2);
  } finally { stop(); }
});
