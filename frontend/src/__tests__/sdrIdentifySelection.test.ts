import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('../render/redraw', () => ({ requestRender: vi.fn() }));
vi.mock('../render/plot', () => ({ plotRect: () => ({ x: 0, y: 0, w: 800, h: 200 }) }));
vi.mock('../ui/graphMode', () => ({ currentGraphMode: () => 'sdr' }));
vi.mock('../ui/spectrumViewport', () => ({ getCapture: () => ({ lo: 100e6, hi: 101e6 }), getView: () => ({ lo: 100e6, hi: 101e6 }) }));
vi.mock('../core/uiScale', () => ({ getUiScale: () => 1 }));
vi.mock('../ui/sdrState', () => ({ sdrIfbw: { get: () => 12000 }, sdrListenHz: { get: () => 100.5e6 } }));
import { beginIdentifySelection, cancelIdentifySelection, identifyBand, identifySelecting, initIdentifySelection, setIdentifySource } from '../ui/sdrIdentifySelection';
let spectrum: HTMLCanvasElement, waterfall: HTMLCanvasElement, input: HTMLButtonElement;
const dispatch = (target: Element, type: string, x = 200, id = 1) => {
  const event = new MouseEvent(type, { clientX: x, clientY: 80, button: 0, bubbles: true, cancelable: true });
  Object.defineProperty(event, 'pointerId', { value: id }); target.dispatchEvent(event); return event;
};
beforeAll(() => initIdentifySelection());
beforeEach(() => {
  document.getElementById = id => Document.prototype.getElementById.call(document, id);
  cancelIdentifySelection(); document.body.innerHTML = '<button id="entry">Select</button><canvas id="spectrum" tabindex="0"></canvas><canvas id="waterfall" tabindex="0"></canvas>';
  spectrum = document.querySelector('#spectrum')!; waterfall = document.querySelector('#waterfall')!; input = document.querySelector('#entry')!;
  for (const canvas of [spectrum, waterfall]) canvas.getBoundingClientRect = () => new DOMRect(0, 0, 800, 200);
  setIdentifySource('source-1', '1', 1000, []);
});
describe('one-shot frequency selection ownership', () => {
  it.each(['spectrum', 'waterfall'])('owns down/move/up on %s before tuning and submits once', surface => {
    const canvas = surface === 'spectrum' ? spectrum : waterfall;
    const tune = vi.fn(), requests = vi.fn(); canvas.addEventListener('pointerdown', tune); document.addEventListener('websa:identify-region', requests);
    input.focus(); beginIdentifySelection();
    dispatch(canvas, 'pointerdown', 200); dispatch(canvas, 'pointermove', 400);
    expect(identifyBand()).toEqual({ low_hz: 100250000, high_hz: 100500000 });
    dispatch(canvas, 'pointerup', 400);
    expect(tune).not.toHaveBeenCalled(); expect(requests).toHaveBeenCalledTimes(1);
    expect(requests.mock.calls[0][0].detail).toMatchObject({ origin: surface, capture_epoch: '1' });
    expect(identifySelecting()).toBe(false); dispatch(canvas, 'pointerdown'); expect(tune).toHaveBeenCalledTimes(1);
    document.removeEventListener('websa:identify-region', requests);
  });
  it('Esc restores focus, consumes the key, and never submits a click', () => {
    const requests = vi.fn(); document.addEventListener('websa:identify-region', requests);
    input.focus(); beginIdentifySelection(); dispatch(spectrum, 'pointerdown');
    const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }); window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true); expect(document.activeElement).toBe(input); expect(identifySelecting()).toBe(false);
    dispatch(spectrum, 'pointerup'); expect(requests).not.toHaveBeenCalled(); document.removeEventListener('websa:identify-region', requests);
  });
  it('cancels on source replacement and pointer cancellation', () => {
    beginIdentifySelection(); dispatch(spectrum, 'pointerdown'); setIdentifySource('source-2', '2', 1000, []); expect(identifySelecting()).toBe(false); expect(identifyBand()).toBeNull();
    beginIdentifySelection(); dispatch(spectrum, 'pointerdown'); dispatch(spectrum, 'pointercancel'); expect(identifySelecting()).toBe(false);
  });
  it('supports keyboard range movement and blocks zoom only during selection', () => {
    beginIdentifySelection(); const previous = identifyBand()!.low_hz;
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true })); expect(identifyBand()!.low_hz).toBeGreaterThan(previous);
    const zoom = vi.fn(); spectrum.addEventListener('wheel', zoom);
    spectrum.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true })); expect(zoom).not.toHaveBeenCalled();
    cancelIdentifySelection(); spectrum.dispatchEvent(new WheelEvent('wheel', { bubbles: true })); expect(zoom).toHaveBeenCalledOnce();
  });
});
