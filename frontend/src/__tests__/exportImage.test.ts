import { afterEach, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { exportSpectrumPng } from '../ui/exportImage';

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

it('exports the spectrum and its interaction layer at the same native pixel coordinates', () => {
  const overlay = document.createElement('canvas'); overlay.id = 'spectrum-overlay';
  overlay.width = S.canvas.width; overlay.height = S.canvas.height; document.body.append(overlay);
  const drawing = { measureText: () => ({ width: 100 }), fillRect: vi.fn(), fillText: vi.fn(), drawImage: vi.fn() };
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(drawing as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, 'toDataURL').mockReturnValue('data:image/png;base64,');
  const download = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
  exportSpectrumPng();
  expect(drawing.drawImage.mock.calls).toEqual([[S.canvas, 12, 26], [overlay, 12, 26]]);
  expect(download).toHaveBeenCalledOnce();
});
