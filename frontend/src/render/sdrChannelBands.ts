import * as S from '../core/store';
import { canvasColors } from '../core/theme';
import { demodBandHz, sdrDemod, sdrPitchHz } from '../ui/sdrState';
import { filterPreviewHz } from '../ui/sdrFilter';
import { currentSdrChannel, sdrChannelMarks, type SdrChannelMark } from '../ui/sdrChannels';
import { sdrHoverFrequency } from '../ui/sdrHoverPassband';
import { sdrPassbandVisible } from '../ui/sdrPassbandState';
import { CHANNEL_COLORS } from '../core/channelColors';

export interface SdrChannelLabel {
  x: number; y: number; width: number; height: number; text: string; selected: boolean;
  mark: SdrChannelMark; indicators: { text: string; color: string; width: number }[];
}

/** Drawing and hit testing share the same packed, absolute-frequency label geometry. */
export function sdrChannelLabels(ctx: CanvasRenderingContext2D, lo: number, hi: number,
  p: { x: number; y: number; w: number; h: number }): SdrChannelLabel[] {
  if (!S.sdrMode || !sdrPassbandVisible.get() || !(hi > lo) || p.w < 40) return [];
  const boxes: SdrChannelLabel[] = [];
  // Leave the 18 px filter grips and corner mode/readout row free for their own gestures.
  const top = p.y + 22;
  const marks = [...sdrChannelMarks()].sort((a, b) => Number(b.channel === currentSdrChannel()) - Number(a.channel === currentSdrChannel()) || a.channel - b.channel);
  ctx.save();
  for (const mark of marks) {
    if (mark.freq < lo || mark.freq > hi) continue;
    const selected = mark.channel === currentSdrChannel();
    ctx.font = `${selected ? 'bold ' : ''}10px monospace`;
    const text = `${mark.channel === 1 ? 'Current' : `C${mark.channel}`} · ${mark.processing_enabled === false ? 'IQ' : mark.mode.toUpperCase()}`;
    const textWidth = ctx.measureText(text).width;
    ctx.font = '9px monospace';
    const indicators = [
      ...(mark.activity?.listening ? [{ text: '●L', color: '#22c982' }] : []),
      ...(mark.activity?.audioRecording ? [{ text: '●R', color: '#ff5252' }] : []),
      ...(mark.activity?.iqRecording ? [{ text: '●IQ', color: '#c78aff' }] : []),
    ].map(icon => ({ ...icon, width: ctx.measureText(icon.text).width + 4 }));
    const width = textWidth + 8 + indicators.reduce((sum, icon) => sum + icon.width, 0), height = 16;
    if (width > p.w - 4) continue;
    const dial = p.x + (mark.freq - lo) / (hi - lo) * p.w;
    let x = Math.max(p.x + 2, Math.min(dial + 2, p.x + p.w - width - 2));
    let y = top;
    const overlaps = (left: number, top: number) => boxes.some(box => top < box.y + box.height + 2 && top + height + 2 > box.y
      && left < box.x + box.width + 3 && left + width + 3 > box.x);
    while (overlaps(x, y)) y += height + 2;
    // Short plots can still fit dense groups by using free space beside a row.
    if (y + height > p.y + p.h) {
      const preferred = x;
      for (y = top; y + height <= p.y + p.h; y += height + 2) {
        const candidates = boxes.filter(box => box.y === y).flatMap(box => [box.x - width - 4, box.x + box.width + 4])
          .filter(left => left >= p.x + 2 && left + width <= p.x + p.w - 2).sort((a, b) => Math.abs(a - preferred) - Math.abs(b - preferred));
        const available = candidates.find(left => !overlaps(left, y));
        if (available !== undefined) { x = available; break; }
      }
    }
    if (y + height <= p.y + p.h) boxes.push({ x, y, width, height, text, selected, mark, indicators });
  }
  ctx.restore();
  return boxes;
}

/** Same absolute-frequency geometry on the spectrum and waterfall. */
export function renderSdrChannelBands(ctx: CanvasRenderingContext2D, lo: number, hi: number,
  p: { x: number; y: number; w: number; h: number }, labels = false): void {
  if (!S.sdrMode || !sdrPassbandVisible.get() || !(hi > lo)) return;
  const xAt = (hz: number) => p.x + (hz - lo) / (hi - lo) * p.w;
  ctx.save(); ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  const marks = [...sdrChannelMarks()].sort((a, b) => Number(b.channel === currentSdrChannel()) - Number(a.channel === currentSdrChannel()));
  for (const channel of marks) {
    const selected = channel.channel === currentSdrChannel();
    const iqBandwidth = channel.baseband?.passband_hz ?? channel.if_bw;
    const [low, high] = channel.processing_enabled === false ? [-iqBandwidth / 2, iqBandwidth / 2]
      : demodBandHz(channel.mode, channel.if_bw, channel.pitch);
    const bandVisible = channel.freq + high >= lo && channel.freq + low <= hi;
    const dialVisible = channel.freq >= lo && channel.freq <= hi;
    if (!bandVisible && !dialVisible) continue;
    const x0 = xAt(channel.freq + low), x1 = xAt(channel.freq + high), dial = xAt(channel.freq);
    const color = CHANNEL_COLORS[(channel.channel - 1) % CHANNEL_COLORS.length];
    if (!selected || channel.processing_enabled === false || !bandVisible) {
      ctx.fillStyle = color; ctx.globalAlpha = channel.enabled ? 0.12 : 0.04;
      if (bandVisible) ctx.fillRect(x0, p.y, Math.max(1, x1 - x0), p.h);
      ctx.globalAlpha = channel.enabled ? 0.8 : 0.4; ctx.strokeStyle = color; ctx.lineWidth = 1;
      ctx.setLineDash(channel.enabled ? [] : [3, 4]);
      if (dialVisible) { ctx.beginPath(); ctx.moveTo(dial, p.y); ctx.lineTo(dial, p.y + p.h); ctx.stroke(); }
    }
    // A passband can intersect the view while its dial is outside it (FT8 is
    // asymmetric). Clamping that dial's label to an edge invents a frequency
    // marker there. Conversely, a visible dial must survive a clipped passband.
  }
  const hover = sdrHoverFrequency();
  if (hover !== null) {
    const [low, high] = demodBandHz(sdrDemod.get(), filterPreviewHz(), sdrPitchHz.get());
    const x0 = xAt(hover + low), x1 = xAt(hover + high);
    const colors = canvasColors();
    ctx.globalAlpha = 1; ctx.fillStyle = colors.hoverPassband;
    ctx.fillRect(x0, p.y, Math.max(1, x1 - x0), p.h);
    ctx.setLineDash([3, 4]);
    ctx.strokeStyle = colors.passbandOutline; ctx.lineWidth = 3;
    ctx.strokeRect(x0, p.y, Math.max(1, x1 - x0), p.h);
    ctx.strokeStyle = colors.hoverPassbandEdge; ctx.lineWidth = 1;
    ctx.strokeRect(x0, p.y, Math.max(1, x1 - x0), p.h);
  }
  if (labels) for (const box of sdrChannelLabels(ctx, lo, hi, p)) {
    ctx.setLineDash([]); ctx.globalAlpha = 0.88; ctx.fillStyle = canvasColors().bg;
    ctx.fillRect(box.x, box.y, box.width, box.height);
    ctx.globalAlpha = 1; ctx.fillStyle = CHANNEL_COLORS[(box.mark.channel - 1) % CHANNEL_COLORS.length];
    ctx.font = `${box.selected ? 'bold ' : ''}10px monospace`; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
    ctx.fillText(box.text, box.x + 4, box.y + 3);
    let x = box.x + 8 + ctx.measureText(box.text).width;
    ctx.font = '9px monospace';
    for (const icon of box.indicators) { ctx.fillStyle = icon.color; ctx.fillText(icon.text, x, box.y + 3); x += icon.width; }
  }
  ctx.restore();
}
