// vitest setup: mock canvas/DOM so the store module (which accesses the spectrum canvas at the top level) can load
// The tests do not do real rendering, only pure logic (smoothing/peak-finding/resampling/normalization)
// - plus every `moveTo`/`lineTo` call that the renderer makes. A test can then assert what a full
// render pass draws. The RTA/SDR trace-polyline regression reads this list.

/** Path calls in order. Call `resetCtxCalls()` before a render pass. */
export const ctxCalls: { op: 'moveTo' | 'lineTo'; x: number; y: number }[] = [];
export const ctxImages: { image: CanvasImageSource; args: number[] }[] = [];
export function resetCtxCalls() { ctxCalls.length = 0; ctxImages.length = 0; }

const fakeCanvas = {
  width: 860,
  height: 480,
  getContext: () => fakeCtx,
  getBoundingClientRect: () => ({ left: 0, top: 0, width: 860, height: 480 }),
  // Code paths that annotate the canvas for e2e/diagnostics (dataset.sdrAudio,
  // dataset.sdrWindow) must not explode in jsdom.
  dataset: {} as Record<string, string>,
  style: {} as Record<string, string>,
  classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
} as unknown as HTMLCanvasElement;

const fakeCtx = new Proxy({ canvas: fakeCanvas }, {
  get: (_target, prop) => {
    // measureText must return a TextMetrics object. The renderer adds its width to a total.
    if (prop === 'measureText') return () => ({ width: 0 });
    // `ctx.canvas` is the back-reference from the spec. The renderers write dataset.* through it.
    if (prop === 'canvas') return fakeCanvas;
    if (prop === 'moveTo' || prop === 'lineTo') {
      return (x: number, y: number) => { ctxCalls.push({ op: prop, x, y }); };
    }
    if (prop === 'drawImage') {
      return (image: CanvasImageSource, ...args: number[]) => { ctxImages.push({ image, args }); };
    }
    return () => {};
  },
  set: () => true,
}) as unknown as CanvasRenderingContext2D;

// Keep the original getElementById, only return the fake canvas for 'spectrum'
const origGetElementById = document.getElementById.bind(document);
document.getElementById = ((id: string) =>
  id === 'spectrum' ? fakeCanvas : origGetElementById(id)) as typeof document.getElementById;

// performance.now fallback (missing in some environments)
if (!globalThis.performance) {
  (globalThis as any).performance = { now: () => 0 };
}

export {};

// The store binds the canvas in initStore() now (report finding P2-1); run it against the
// fake canvas so the modules under test have a context and logical dimensions.
import { initStore } from '../core/store';
initStore();
