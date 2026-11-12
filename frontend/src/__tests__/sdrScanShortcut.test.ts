import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('../ui/nativeWorkbench', () => ({ nativeWorkbenchSession: vi.fn(async () => null) }));
vi.mock('../ui/keypad', () => ({ isKeypadOpen: vi.fn(() => false) }));
vi.mock('../ui/sdrIdentifySelection', () => ({
  initIdentifySelection: vi.fn(), beginIdentifySelection: vi.fn(), highlightIdentifyBand: vi.fn(),
  identifySelecting: vi.fn(), setIdentifySource: vi.fn(),
}));

const cleanups: Array<() => void> = [];
beforeEach(async () => {
  vi.resetModules(); vi.clearAllMocks();
  document.body.innerHTML = '<button id="sdr-scan-toggle"></button>';
  const add = document.addEventListener;
  vi.spyOn(document, 'addEventListener').mockImplementation((type, listener, options) => {
    add.call(document, type, listener, options);
    cleanups.push(() => document.removeEventListener(type, listener, options));
  });
  const { deviceMode, graphMode } = await import('../ui/graphMode');
  deviceMode.confirm('sdr'); graphMode.confirm('sdr');
  (await import('../ui/sdrScan')).initSdrScan();
});
afterEach(() => { cleanups.splice(0).forEach(fn => fn()); vi.restoreAllMocks(); document.body.replaceChildren(); });

function key(options: KeyboardEventInit = {}) {
  const event = new KeyboardEvent('keydown', { key: 's', bubbles: true, cancelable: true, ...options });
  (document.activeElement ?? document).dispatchEvent(event);
  return event;
}

it('uses the same lazy window opener as the scan button and ignores held keys', async () => {
  expect(key().defaultPrevented).toBe(true);
  await vi.dynamicImportSettled();
  const { nativeWorkbenchSession } = await import('../ui/nativeWorkbench');
  expect(nativeWorkbenchSession).toHaveBeenCalledOnce();
  expect(key({ repeat: true }).defaultPrevented).toBe(true);
  await vi.dynamicImportSettled();
  expect(nativeWorkbenchSession).toHaveBeenCalledOnce();
  expect(document.querySelector('#sdr-scan-toggle')?.getAttribute('aria-keyshortcuts')).toBe('S');
});

it.each(['input', 'textarea', 'select', 'editable', 'dialog'])('leaves S to the focused %s', async kind => {
  const node = document.createElement(kind === 'editable' ? 'div' : kind === 'dialog' ? 'button' : kind);
  if (kind === 'editable') { node.setAttribute('contenteditable', 'true'); node.tabIndex = 0; }
  if (kind === 'dialog') {
    const dialog = document.createElement('dialog'); dialog.open = true; dialog.append(node); document.body.append(dialog);
  } else document.body.append(node);
  node.focus();
  expect(key().defaultPrevented).toBe(false);
  await vi.dynamicImportSettled();
  expect((await import('../ui/nativeWorkbench')).nativeWorkbenchSession).not.toHaveBeenCalled();
});

it.each([{ ctrlKey: true }, { altKey: true }, { metaKey: true }, { shiftKey: true }, { isComposing: true }])('ignores modifier/composition input %o', async options => {
  expect(key(options).defaultPrevented).toBe(false);
  await vi.dynamicImportSettled();
  expect((await import('../ui/nativeWorkbench')).nativeWorkbenchSession).not.toHaveBeenCalled();
});

it('ignores S outside SDR and while the numeric keypad is open', async () => {
  const { graphMode } = await import('../ui/graphMode');
  graphMode.confirm('std');
  expect(key().defaultPrevented).toBe(false);
  graphMode.confirm('sdr');
  vi.mocked((await import('../ui/keypad')).isKeypadOpen).mockReturnValue(true);
  expect(key().defaultPrevented).toBe(false);
  await vi.dynamicImportSettled();
  expect((await import('../ui/nativeWorkbench')).nativeWorkbenchSession).not.toHaveBeenCalled();
});
