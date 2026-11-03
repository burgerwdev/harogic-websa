import { afterEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { renderAll } from '../render/spectrum';
import { plotRect } from '../render/plot';
import { axisDragMove, axisDragStart, resetAxisPreview } from '../ui/axisDrag';
import { graphMode } from '../ui/graphMode';
import { cancelMarqueePreview, updateMarqueePreview } from '../ui/spectrumZoomGestures';
import { __testSeedCapture, setView, zoomEnabled } from '../ui/spectrumViewport';
import { ctxRects, resetCtxCalls } from './setup';

afterEach(() => {
  cancelMarqueePreview();
  resetAxisPreview();
  __testSeedCapture(null);
  S.setFreqArray(null);
});

describe('spectrum selection drawing during a frequency-axis preview', () => {
  it.each([false, true])('keeps the rectangle under the pointer with existing zoom %s', zoom => {
    resetAll();
    graphMode.confirm('std');
    S.setRtaMode(false);
    S.setSdrMode(false);
    S.setFreqArray(Float64Array.from({ length: 101 }, (_, i) => 900e6 + i * 1e6));
    __testSeedCapture({ lo: 900e6, hi: 1000e6 });
    zoomEnabled.set(true);
    if (zoom) setView({ lo: 940e6, hi: 960e6 }, 1);
    const rect = plotRect();
    axisDragStart('x', 400, rect.y + rect.h + 10, rect);
    axisDragMove(600, rect.y + rect.h + 10);
    updateMarqueePreview(210, 410);
    resetCtxCalls();
    renderAll();
    expect(ctxRects).toContainEqual([210, rect.y, 200, rect.h]);
  });
});
