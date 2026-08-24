// vitest setup: mock canvas/DOM, 供 store 模块(顶层访问 spectrum canvas)加载
// 测试不涉及真实渲染, 只测纯逻辑(平滑/寻峰/重采样/归一化)
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

// 保留原 getElementById, 仅对 spectrum 返回 fake canvas
const origGetElementById = document.getElementById.bind(document);
document.getElementById = ((id: string) =>
  id === 'spectrum' ? fakeCanvas : origGetElementById(id)) as typeof document.getElementById;

// performance.now 兜底(某些环境缺失)
if (!globalThis.performance) {
  (globalThis as any).performance = { now: () => 0 };
}

export {};
