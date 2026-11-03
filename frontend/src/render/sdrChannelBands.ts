import * as S from '../core/store';
import { canvasColors } from '../core/theme';
import { demodBandHz, sdrDemod, sdrPitchHz } from '../ui/sdrState';
import { filterPreviewHz } from '../ui/sdrFilter';
import { currentSdrChannel, sdrChannelMarks } from '../ui/sdrChannels';
import { sdrHoverFrequency } from '../ui/sdrHoverPassband';

const channelColors = ['#6ecfa4', '#e6b95c', '#bc9bf3'];
/** Same absolute-frequency geometry on the spectrum and waterfall. */
export function renderSdrChannelBands(ctx: CanvasRenderingContext2D, lo: number, hi: number,
  p: { x: number; y: number; w: number; h: number }, labels = false): void {
  if (!S.sdrMode || !(hi > lo)) return;
  const xAt = (hz: number) => p.x + (hz - lo) / (hi - lo) * p.w;
  ctx.save(); ctx.beginPath(); ctx.rect(p.x, p.y, p.w, p.h); ctx.clip();
  for (const channel of sdrChannelMarks()) {
    if (channel.channel === currentSdrChannel()) continue;
    const iqBandwidth = channel.baseband?.passband_hz ?? channel.if_bw;
    const [low, high] = channel.processing_enabled === false ? [-iqBandwidth / 2, iqBandwidth / 2]
      : demodBandHz(channel.mode, channel.if_bw, channel.pitch);
    if (channel.freq + high < lo || channel.freq + low > hi) continue;
    const x0 = xAt(channel.freq + low), x1 = xAt(channel.freq + high), dial = xAt(channel.freq);
    const color = channelColors[channel.channel - 1];
    ctx.fillStyle = color; ctx.globalAlpha = channel.enabled ? 0.12 : 0.04;
    ctx.fillRect(x0, p.y, Math.max(1, x1 - x0), p.h);
    ctx.globalAlpha = channel.enabled ? 0.8 : 0.4; ctx.strokeStyle = color; ctx.lineWidth = 1;
    ctx.setLineDash(channel.enabled ? [] : [3, 4]);
    ctx.beginPath(); ctx.moveTo(dial, p.y); ctx.lineTo(dial, p.y + p.h); ctx.stroke();
    if (labels) {
      ctx.font = '10px monospace'; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
      const text = `C${channel.channel} · ${channel.processing_enabled === false ? 'IQ' : channel.mode.toUpperCase()}`;
      const x = Math.max(p.x + 3, Math.min(dial + 4, p.x + p.w - ctx.measureText(text).width - 4));
      ctx.globalAlpha = 0.85; ctx.fillStyle = canvasColors().bg;
      ctx.fillRect(x - 2, p.y + 3, ctx.measureText(text).width + 4, 13);
      ctx.globalAlpha = 1; ctx.fillStyle = color; ctx.fillText(text, x, p.y + 4);
    }
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
  ctx.restore();
}
