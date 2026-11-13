import { afterEach, expect, it, vi } from 'vitest';
import { batchRender, limitStreamRendering, requestRender, requestStreamRender, setInteractionRenderer, setRenderer, streamRenderInterval } from '../render/redraw';

afterEach(() => { setRenderer(null); setInteractionRenderer(null); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it('reserves display time for concurrent players without delaying user actions, then restores cadence', () => {
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  const paint = vi.fn(); setRenderer(paint);
  const releaseFirst = limitStreamRendering(5), releaseSecond = limitStreamRendering(10);
  try {
    expect(streamRenderInterval()).toBe(200);
    requestRender(); expect(paint).toHaveBeenCalledOnce();
    releaseFirst(); releaseFirst();
    expect(streamRenderInterval()).toBe(100);
  } finally { releaseFirst(); releaseSecond(); }
  expect(streamRenderInterval()).toBeLessThan(100);
});

it('keeps incoming spectrum paints scheduled while independently painting pointer overlays', async () => {
  vi.useFakeTimers(); vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  const frames = new Map<number, FrameRequestCallback>(); let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { frames.set(++sequence, fn); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
  const paint = vi.fn(), overlay = vi.fn(() => true);
  setRenderer(paint); setInteractionRenderer(overlay);
  requestStreamRender(); await vi.advanceTimersByTimeAsync(0);
  for (let i = 0; i < 10; i++) batchRender(requestRender, true);
  expect(overlay).toHaveBeenCalledTimes(10); expect(paint).not.toHaveBeenCalled();
  expect(frames.size).toBe(1);
  [...frames.values()][0](performance.now()); expect(paint).toHaveBeenCalledOnce();
  overlay.mockReturnValue(false);
  batchRender(requestRender, true); expect(paint).toHaveBeenCalledTimes(2);
});

it('paints all pointer state together once, including nested and interrupted updates', () => {
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  let cursor = 0, passband = 0;
  const paint = vi.fn(() => [cursor, passband]); setRenderer(paint);
  batchRender(() => {
    cursor = 1; requestRender();
    batchRender(() => { passband = 2; requestRender(); });
    expect(paint).not.toHaveBeenCalled();
  });
  expect(paint).toHaveBeenCalledOnce(); expect(paint).toHaveReturnedWith([1, 2]);
  expect(() => batchRender(() => { requestRender(); throw Error('cancel'); })).toThrow('cancel');
  requestRender(); expect(paint).toHaveBeenCalledTimes(3);
});

it('bounds pending display work, adapts to paint cost and renders the newest ingested state', async () => {
  vi.useFakeTimers(); vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  const frames = new Map<number, FrameRequestCallback>(); let sequence = 0, clock = 0, state = 0;
  vi.spyOn(performance, 'now').mockImplementation(() => clock);
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { frames.set(++sequence, fn); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
  const paint = vi.fn(() => { clock += 30; return state; }); setRenderer(paint);
  const flushFrame = (): void => { const pending = [...frames.values()]; frames.clear(); pending.forEach(fn => fn(clock)); };
  for (state = 0; state < 100; state++) requestStreamRender();
  expect(paint).not.toHaveBeenCalled(); await vi.advanceTimersByTimeAsync(0);
  expect(frames.size).toBe(1); flushFrame(); expect(paint).toHaveReturnedWith(100);
  clock = 31;
  for (state = 100; state < 200; state++) requestStreamRender();
  await vi.advanceTimersByTimeAsync(68); expect(frames.size).toBe(0);
  clock = 100; await vi.advanceTimersByTimeAsync(1); expect(frames.size).toBe(1);
  flushFrame(); expect(paint).toHaveBeenCalledTimes(2); expect(paint).toHaveLastReturnedWith(200);
  await vi.advanceTimersByTimeAsync(1000); expect(frames.size).toBe(0);
});

it('paints user actions synchronously and cancels a redundant queued data repaint', async () => {
  vi.useFakeTimers(); vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  const frames = new Map<number, FrameRequestCallback>(); let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { frames.set(++sequence, fn); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
  const paint = vi.fn(); setRenderer(paint);
  requestStreamRender(); await vi.advanceTimersByTimeAsync(0); expect(frames.size).toBe(1);
  requestRender(); expect(paint).toHaveBeenCalledOnce(); expect(frames.size).toBe(0);
});

it('coalesces background redraws and paints the latest state once on foregrounding', () => {
  const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
  let value = 0;
  const paint = vi.fn(() => value);
  setRenderer(paint);
  for (value = 0; value < 100; value++) requestRender();
  expect(paint).not.toHaveBeenCalled();
  hidden.mockReturnValue(false); document.dispatchEvent(new Event('visibilitychange'));
  expect(paint).toHaveBeenCalledOnce(); expect(paint).toHaveReturnedWith(100);
  document.dispatchEvent(new Event('visibilitychange')); expect(paint).toHaveBeenCalledOnce();
});

it('keeps normal 5 ms spectrum paints at the 30 Hz display cadence', async () => {
  vi.useFakeTimers(); vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  let clock = 0;
  vi.spyOn(performance, 'now').mockImplementation(() => clock);
  const frames: FrameRequestCallback[] = [];
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { frames.push(fn); return frames.length; });
  vi.stubGlobal('cancelAnimationFrame', () => {});
  const paint = vi.fn(() => { clock += 5; }); setRenderer(paint);
  requestStreamRender(); await vi.advanceTimersByTimeAsync(0); frames.shift()!(clock);
  requestStreamRender(); await vi.advanceTimersByTimeAsync(29);
  expect(frames).toHaveLength(1); clock = 34; frames.shift()!(clock);
  expect(paint).toHaveBeenCalledTimes(2);
});
