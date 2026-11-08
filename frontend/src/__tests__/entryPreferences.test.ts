import { afterEach, beforeEach, expect, it, vi } from 'vitest';

// Exercise the real entry preference initialization and button actions without
// starting sockets, timers in DOM controls, or a drawing loop.
vi.mock('../core/ws', () => ({ connectWS: vi.fn() }));
vi.mock('../core/store', () => ({ initStore: vi.fn(), viewMode: 'std' }));
vi.mock('../core/units', () => ({ buildUnitGroups: vi.fn() }));
vi.mock('../core/level', () => ({ initLevelUnit: vi.fn() }));
vi.mock('../render/spectrum', () => ({}));
vi.mock('../render/markerTable', () => ({ initMarkerTable: vi.fn() }));
vi.mock('../render/redraw', () => ({ requestRender: vi.fn(), renderRequestCount: vi.fn() }));
vi.mock('../render/infobar', () => ({ updateInfoBar: vi.fn() }));
vi.mock('../meas/channel', () => ({ updateChanTable: vi.fn() }));
vi.mock('../ui/limits', () => ({ initLimits: vi.fn(), refreshLimitUnits: vi.fn() }));
vi.mock('../ui/controlRail', () => ({ initControlRail: vi.fn() }));
vi.mock('../ui/trigger', () => ({ initTrigger: vi.fn() }));
vi.mock('../ui/wfRange', () => ({ initWfRange: vi.fn() }));
vi.mock('../ui/keypad', () => ({ initKeypad: vi.fn() }));
vi.mock('../ui/popoverDrag', () => ({ initPopoverDrag: vi.fn() }));
vi.mock('../ui/spectrumZoomUi', () => ({ initSpectrumZoomUi: vi.fn() }));
vi.mock('../ui/controls', () => ({ bindActions: vi.fn(), bindCanvas: vi.fn(), syncToggleIcons: vi.fn(), syncToggleTexts: vi.fn() }));
vi.mock('../audio/sdrAudio', () => ({ sampleEnvelope: vi.fn(), sampleSpectrum: vi.fn() }));

beforeEach(() => {
  vi.resetModules(); vi.useFakeTimers();
  document.body.innerHTML = '<button id="btn-theme"></button><button id="btn-lang"></button>';
});
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); document.body.replaceChildren(); });

it('starts the control connection when browser preference storage is inaccessible', async () => {
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new DOMException('denied', 'SecurityError'); });
  await expect(import('../main')).resolves.toBeDefined();
  const { connectWS } = await import('../core/ws');
  expect(connectWS).toHaveBeenCalledOnce();
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(document.documentElement.lang).toBe('en');
});

it('finishes theme and language actions when preference storage is full', async () => {
  await import('../main');
  const errors: unknown[] = [];
  const failed = (event: ErrorEvent) => { errors.push(event.error); event.preventDefault(); };
  window.addEventListener('error', failed);
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError'); });
  try {
    document.querySelector<HTMLButtonElement>('#btn-theme')!.click();
    document.querySelector<HTMLButtonElement>('#btn-lang')!.click();
    expect(errors).toEqual([]);
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(document.documentElement.lang).toBe('zh-CN');
    expect(document.querySelector('#btn-lang')!.textContent).toBe('中文');
  } finally { window.removeEventListener('error', failed); }
});
