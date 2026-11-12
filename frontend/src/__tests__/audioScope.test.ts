import { afterEach, expect, it, vi } from 'vitest';
import { drawAudioScope } from '../ui/audioScope';
import { setTheme } from '../core/theme';

afterEach(() => { vi.restoreAllMocks(); setTheme('dark'); });

it('resolves scope colours on theme/document changes instead of on every audio packet', () => {
  const canvas = document.createElement('canvas');
  const context = { clearRect: vi.fn(), beginPath: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), strokeStyle: '' };
  vi.spyOn(canvas, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
  const styles = vi.spyOn(window, 'getComputedStyle').mockImplementation(() => ({ getPropertyValue: (name: string) =>
    name === '--text' ? document.documentElement.dataset.theme === 'light' ? '#112233' : '#ccddee' : '#444444' }) as CSSStyleDeclaration);
  const audio = [Float32Array.of(0, .1, 0, -.1)];
  setTheme('dark');
  for (let i = 0; i < 100; i++) drawAudioScope(canvas, audio, 1);
  expect(styles).toHaveBeenCalledTimes(1); expect(context.strokeStyle).toBe('#ccddee');
  setTheme('light'); drawAudioScope(canvas, audio, 1);
  expect(styles).toHaveBeenCalledTimes(2); expect(context.strokeStyle).toBe('#112233');
  document.implementation.createHTMLDocument().adoptNode(canvas); drawAudioScope(canvas, audio, 1);
  expect(styles).toHaveBeenCalledTimes(3);
});

it('retries scope colours when a new window has not loaded its stylesheet yet', () => {
  const canvas = document.createElement('canvas');
  const context = { clearRect: vi.fn(), beginPath: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), strokeStyle: '' };
  vi.spyOn(canvas, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
  let loaded = false;
  const styles = vi.spyOn(window, 'getComputedStyle').mockImplementation(() => ({ getPropertyValue: () => loaded ? '#123456' : '' }) as unknown as CSSStyleDeclaration);
  const audio = [Float32Array.of(0, .1)];
  drawAudioScope(canvas, audio, 1); expect(context.strokeStyle).toBe('#ccc');
  loaded = true;
  drawAudioScope(canvas, audio, 1); drawAudioScope(canvas, audio, 1);
  expect(styles).toHaveBeenCalledTimes(2); expect(context.strokeStyle).toBe('#123456');
});

it('retains time-ordered impulses with a bounded number of canvas vertices', () => {
  const canvas = document.createElement('canvas'); canvas.width = 40; canvas.height = 80;
  const paths: [number, number][] = [];
  const context = { clearRect() {}, beginPath() {}, moveTo: (x: number, y: number) => paths.push([x, y]),
    lineTo: (x: number, y: number) => paths.push([x, y]), stroke() {} };
  vi.spyOn(canvas, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
  const samples = new Float32Array(48000); samples[12345] = 1; samples[12346] = -1;
  drawAudioScope(canvas, [samples], 1);
  const trace = paths.slice(2);
  expect(trace.some(([, y]) => y === 2)).toBe(true);
  expect(trace.some(([, y]) => y === 78)).toBe(true);
  expect(trace.length).toBeLessThanOrEqual(canvas.width * 4);
  expect(trace.every(([x, y], i) => Number.isFinite(x + y) && (!i || x >= trace[i - 1][0]))).toBe(true);
  expect(trace.at(-1)![0]).toBe(39);
});

it('lifts the pen at missing samples and handles a single-sample lane', () => {
  const canvas = document.createElement('canvas'); canvas.width = 80; canvas.height = 80;
  const context = { clearRect: vi.fn(), beginPath: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn() };
  vi.spyOn(canvas, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
  drawAudioScope(canvas, [Float32Array.of(1, NaN, -1)], 1);
  expect(context.moveTo).toHaveBeenCalledWith(0, 2);
  expect(context.moveTo).toHaveBeenCalledWith(79, 78);
  expect(context.lineTo).toHaveBeenCalledTimes(1);
  context.moveTo.mockClear();
  drawAudioScope(canvas, [Float32Array.of(0.5)], 1);
  expect(context.moveTo).toHaveBeenCalledWith(0, 21);
  drawAudioScope(canvas, [], 1);
});
