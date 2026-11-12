import { afterEach, beforeEach, expect, it, vi } from 'vitest';

beforeEach(() => { vi.resetModules(); localStorage.clear(); });
afterEach(() => { vi.restoreAllMocks(); document.body.replaceChildren(); });

function surface(doc: Document = document) {
  const root = doc.createElement('section'), slider = doc.createElement('input');
  root.append(slider); doc.body.append(root);
  return { root, slider };
}

it('synchronizes existing, newly opened and popped-out windows through one stored preference', async () => {
  const { bindWindowOpacity } = await import('../ui/windowOpacity');
  const first = surface(), second = surface(document.implementation.createHTMLDocument('Popout'));
  const off = bindWindowOpacity(first.root, first.slider);
  const off2 = bindWindowOpacity(second.root, second.slider);
  first.slider.value = '60'; first.slider.dispatchEvent(new Event('input'));
  expect(second.root.style.opacity).toBe('0.6');
  expect(second.slider.value).toBe('60');
  expect(localStorage.getItem('web-sa-window-opacity')).toBe('0.6');
  const later = surface(), off3 = bindWindowOpacity(later.root, later.slider);
  expect(later.slider.value).toBe('60');
  second.slider.value = '100'; second.slider.dispatchEvent(new Event('input'));
  for (const view of [first, second, later]) {
    expect(view.root.style.opacity).toBe('1');
    expect(view.root.dataset.windowOpacity).toBe('opaque');
    expect(view.slider.value).toBe('100');
  }
  off(); off2(); off3();
});

it('migrates the old Decode preference once and restores the global value after reload', async () => {
  localStorage.setItem('websa-decode-window', JSON.stringify({ opacity: .65 }));
  let controller = await import('../ui/windowOpacity');
  const first = surface(), off = controller.bindWindowOpacity(first.root, first.slider);
  expect(first.slider.value).toBe('65');
  controller.setWindowOpacity(.8); off();
  vi.resetModules(); controller = await import('../ui/windowOpacity');
  const second = surface(), off2 = controller.bindWindowOpacity(second.root, second.slider);
  expect(second.slider.value).toBe('80');
  off2();
});

it('keeps controls usable when storage is denied and releases closed surfaces', async () => {
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw Error('denied'); });
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw Error('denied'); });
  const { bindWindowOpacity, setWindowOpacity } = await import('../ui/windowOpacity');
  const first = surface(), off = bindWindowOpacity(first.root, first.slider);
  expect(first.slider.value).toBe('100');
  setWindowOpacity(0);
  expect(first.root.style.opacity).toBe('0.35');
  off(); setWindowOpacity(.8);
  expect(first.root.style.opacity).toBe('');
  expect(first.slider.value).toBe('35');
});
