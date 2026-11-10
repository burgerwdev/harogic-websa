import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { renderSdrChannelBands, sdrChannelLabels } from '../render/sdrChannelBands';
import { channelControlContext, confirmSdrChannels, confirmSdrReceiverNumbers, setCurrentSdrChannel } from '../ui/sdrChannels';
import { renderRequestCount } from '../render/redraw';
import { sdrPassbandVisible } from '../ui/sdrPassbandState';
import { setPluginManifest } from '../sdr/registry';
import { receiverPlugin } from './pluginFixtures';

const plot = { x: 20, y: 10, w: 800, h: 300 };
const dial = 14_074_000;
const context = () => ({
  save: vi.fn(), restore: vi.fn(), beginPath: vi.fn(), rect: vi.fn(), clip: vi.fn(),
  fillRect: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), setLineDash: vi.fn(),
  fillText: vi.fn(), measureText: (text: string) => ({ width: text.length * 6 }),
});

beforeEach(() => {
  S.setSdrMode(true); sdrPassbandVisible.set(true); setCurrentSdrChannel(1);
  confirmSdrReceiverNumbers(null);
  setPluginManifest(['ft8', 'am'].map(id => receiverPlugin(id)));
  confirmSdrChannels([{ channel: 2, freq: dial, mode: 'ft8', if_bw: 3000, enabled: true }]);
});
afterEach(() => { confirmSdrChannels([]); S.setSdrMode(false); setPluginManifest(null); });

describe('absolute channel dial positions', () => {
  it.each([300, 80])('packs sixteen nearby labels in a %i px plot without overlapping their click targets', height => {
    confirmSdrChannels(Array.from({ length: 16 }, (_, i) => ({ channel: i + 1, freq: dial + i, mode: 'am', if_bw: 6000, enabled: true })));
    confirmSdrReceiverNumbers(Array.from({ length: 16 }, (_, i) => i + 1), undefined,
      Array.from({ length: 16 }, (_, i) => ({ channel: i + 1, listening: true, audioRecording: true, iqRecording: true })));
    const ctx = context() as unknown as CanvasRenderingContext2D;
    const boxes = sdrChannelLabels(ctx, dial - 1000, dial + 1000, { ...plot, h: height });
    expect(boxes).toHaveLength(16);
    expect(boxes[0].indicators.map(icon => icon.text)).toEqual(['●L', '●R', '●IQ']);
    for (const box of boxes) {
      expect(box.y).toBeGreaterThan(plot.y + 18);
      expect(box.x).toBeGreaterThanOrEqual(plot.x);
      expect(box.x + box.width).toBeLessThanOrEqual(plot.x + plot.w);
      for (const other of boxes.filter(other => other !== box))
        expect(box.x + box.width <= other.x || other.x + other.width <= box.x
          || box.y + box.height <= other.y || other.y + other.height <= box.y).toBe(true);
    }
    const before = renderRequestCount();
    confirmSdrReceiverNumbers(Array.from({ length: 16 }, (_, i) => i + 1));
    expect(renderRequestCount()).toBeGreaterThan(before);
    expect(sdrChannelLabels(ctx, dial - 1000, dial + 1000, plot)[0].indicators).toEqual([]);
  });
  it('refreshes edit tokens without repainting unchanged channel geometry', () => {
    const mark = { channel: 2, freq: dial, mode: 'ft8', if_bw: 3000, enabled: true,
      control: { service_id: 'service', receiver_uid: 'receiver', revision: '1', action_token: 'first' } };
    confirmSdrChannels([mark]);
    const before = renderRequestCount();
    confirmSdrChannels([{ ...mark, control: { ...mark.control, revision: '2', action_token: 'second' } }]);
    expect(renderRequestCount()).toBe(before);
    expect(channelControlContext(2)).toMatchObject({ expected_revision: '2', action_token: 'second' });
    confirmSdrChannels([{ ...mark, freq: dial + 1000 }]);
    expect(renderRequestCount()).toBe(before + 1);
  });
  it.each([
    { lo: dial - 10000, hi: dial + 10000 },
    { lo: dial - 4000, hi: dial + 1000 },
    { lo: dial - 500, hi: dial + 500 },
  ])('keeps the dial at its RF coordinate after pan/zoom to %j', view => {
    const ctx = context();
    renderSdrChannelBands(ctx as unknown as CanvasRenderingContext2D, view.lo, view.hi, plot, true);
    const x = plot.x + (dial - view.lo) / (view.hi - view.lo) * plot.w;
    expect(ctx.moveTo).toHaveBeenCalledWith(x, plot.y);
    expect(ctx.lineTo).toHaveBeenCalledWith(x, plot.y + plot.h);
    expect(ctx.fillText).toHaveBeenCalledWith('C2 · FT8', expect.any(Number), expect.any(Number));
  });

  it('retains the visible passband without pinning an off-screen dial label to the edge', () => {
    const ctx = context();
    renderSdrChannelBands(ctx as unknown as CanvasRenderingContext2D, dial + 1000, dial + 2000, plot, true);
    expect(ctx.fillRect).toHaveBeenCalled();
    expect(ctx.fillText).not.toHaveBeenCalled();
    expect(ctx.moveTo).not.toHaveBeenCalled();
  });

  it('shows the FT8 dial at the right edge even when its search band is outside the view', () => {
    const ctx = context();
    renderSdrChannelBands(ctx as unknown as CanvasRenderingContext2D, dial - 1000, dial + 50, plot, true);
    const x = plot.x + 1000 / 1050 * plot.w;
    expect(ctx.moveTo).toHaveBeenCalledWith(x, plot.y);
    expect(ctx.fillText).toHaveBeenCalledWith('C2 · FT8', expect.any(Number), expect.any(Number));
  });

  it('does not create an AM dial label when only the edge of its band remains visible', () => {
    confirmSdrChannels([{ channel: 2, freq: dial, mode: 'am', if_bw: 6000, enabled: true }]);
    const ctx = context();
    renderSdrChannelBands(ctx as unknown as CanvasRenderingContext2D, dial - 2000, dial - 1000, plot, true);
    expect(ctx.fillRect).toHaveBeenCalled();
    expect(ctx.fillText).not.toHaveBeenCalled();
  });
});
