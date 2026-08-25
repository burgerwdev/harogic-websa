// vitest setup: mock canvas/DOM so the store module (which accesses the spectrum canvas at the top level) can load
// The tests do not do real rendering, only pure logic (smoothing/peak-finding/resampling/normalization)
const fakeCtx = new Proxy({}, {
  get: () => () => {},
  set: () => true,
}) as unknown as CanvasRenderingContext2D;

const fakeCanvas = {
  width: 860,
  height: 480,
  getContext: () => fakeCtx,
  getBoundingClientRect: () => ({ left: 0, top: 0, width: 860, height: 480 }),
} as unknown as HTMLCanvasElement;

// Keep the original getElementById, only return the fake canvas for 'spectrum'
const origGetElementById = document.getElementById.bind(document);
document.getElementById = ((id: string) =>
  id === 'spectrum' ? fakeCanvas : origGetElementById(id)) as typeof document.getElementById;

// performance.now fallback (missing in some environments)
if (!globalThis.performance) {
  (globalThis as any).performance = { now: () => 0 };
}

export {};
