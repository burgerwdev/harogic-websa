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
