import { afterEach, expect, it, vi } from 'vitest';
import { CanvasLayer } from '../render/canvasLayer';

afterEach(() => vi.restoreAllMocks());
it('reuses label pixels and repaints on theme, geometry or pixel ratio changes', () => {
  const drawing = { setTransform: vi.fn(), clearRect: vi.fn() }, target = { drawImage: vi.fn() }, paint = vi.fn();
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(drawing as unknown as CanvasRenderingContext2D);
  const layer = new CanvasLayer();
  const draw = (key = 'dark', width = 100, ratio = 1) => layer.draw(target as unknown as CanvasRenderingContext2D, key, 10, 20, width, 18, ratio, paint);
  draw(); draw(); draw(); expect(paint).toHaveBeenCalledOnce(); expect(target.drawImage).toHaveBeenCalledTimes(3);
  draw('light'); draw('light', 120); draw('light', 120, 2); expect(paint).toHaveBeenCalledTimes(4);
  expect(drawing.setTransform).toHaveBeenLastCalledWith(2, 0, 0, 2, 0, 0);
  draw('light', 120, 2); expect(paint).toHaveBeenCalledTimes(4);
});
