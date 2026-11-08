import { afterEach, describe, expect, it, vi } from 'vitest';
import { createDecodePopout } from '../ui/decodePopout';
import { setLang } from '../core/i18n';
import { setTheme } from '../core/theme';
import { setUiScale } from '../core/uiScale';

afterEach(() => { vi.restoreAllMocks(); document.body.replaceChildren(); setLang('en'); setTheme('dark'); setUiScale(1); });
function surface(): HTMLElement {
  const root = document.createElement('div');
  root.innerHTML = '<button id="btn-decode-popout" data-i18n="decode_popout"></button><button id="btn-decode-dock" hidden></button><p id="decode-popout-error" hidden></p><canvas></canvas>';
  document.body.appendChild(root);
  return root;
}
describe('Decode surface portal', () => {
  it('retains the live surface and reports blocked popup visibly', () => {
    const root = surface();
    vi.spyOn(window, 'open').mockReturnValue(null);
    const before = vi.fn(), after = vi.fn();
    const portal = createDecodePopout(root, before, after);
    portal.popout();
    expect(root.ownerDocument).toBe(document);
    expect(root.querySelector<HTMLElement>('#decode-popout-error')!.hidden).toBe(false);
    expect(root.textContent).toContain('Allow popups');
    expect(before).not.toHaveBeenCalled();
    expect(portal.isPoppedOut()).toBe(false);
  });
  it('moves the same canvas, synchronizes preferences and docks on child close', () => {
    const root = surface();
    const canvas = root.querySelector('canvas');
    const click = vi.fn();
    canvas!.addEventListener('click', click);
    const iframe = document.createElement('iframe');
    document.body.appendChild(iframe);
    const child = iframe.contentWindow!;
    vi.spyOn(child, 'close').mockImplementation(() => {});
    vi.spyOn(child, 'focus').mockImplementation(() => {});
    const open = vi.spyOn(window, 'open').mockReturnValue(child);
    const portal = createDecodePopout(root, vi.fn(), vi.fn());
    portal.popout();
    expect(root.ownerDocument).toBe(child.document);
    expect(root.querySelector('canvas')).toBe(canvas);
    canvas!.click();
    expect(click).toHaveBeenCalledTimes(1);
    portal.popout();
    expect(open).toHaveBeenCalledTimes(1);
    setTheme('light');
    setLang('zh');
    setUiScale(1.2);
    expect(child.document.documentElement.dataset.theme).toBe('light');
    expect(child.document.documentElement.lang).toBe('zh-CN');
    expect(root.querySelector('#btn-decode-popout')?.textContent).toBe('弹出');
    expect(root.style.zoom).toBe('1.2');
    child.dispatchEvent(new Event('beforeunload'));
    expect(root.ownerDocument).toBe(document);
    expect(root.querySelector('canvas')).toBe(canvas);
    expect(portal.isPoppedOut()).toBe(false);
  });
});
