import { expect, it, vi } from 'vitest';
vi.mock('../core/theme', () => ({ canvasColors: () => ({ bg: '#000', text: '#fff' }) }));
vi.mock('../ui/graphMode', () => ({ currentGraphMode: () => 'sdr' }));
vi.mock('../ui/sdrIdentifySelection', () => ({ identifyBand: () => ({ low_hz: 9.8e6, high_hz: 9.9e6 }), identifySelecting: () => true, selectionHint: () => 'Drag band · Esc cancels' }));
import { renderIdentifyBand } from '../render/sdrIdentifyBand';
it('keeps traces visible through a compact hint inside the plot', () => {
  const fills: { alpha: number; x: number; width: number }[] = [];
  const ctx = { globalAlpha: 1, save() {}, restore() {}, beginPath() {}, rect() {}, clip() {}, moveTo() {}, lineTo() {}, stroke() {}, measureText: () => ({ width: 180 }), fillText() {}, fillRect(x: number, _y: number, width: number) { fills.push({ alpha: this.globalAlpha, x, width }); } };
  renderIdentifyBand(ctx as unknown as CanvasRenderingContext2D, 9e6, 10e6, { x: 10, y: 0, w: 240, h: 100 }, true);
  const hint = fills.at(-1)!; expect(hint.alpha).toBeLessThan(.5); expect(hint.width).toBeLessThan(240); expect(hint.x + hint.width).toBeLessThanOrEqual(250);
});
