import { isChannelNumber } from '../core/channelState';
import { requestRender } from '../render/redraw';

export type SdrChannel = number;
export interface SdrChannelMark {
  channel: SdrChannel;
  freq: number;
  mode: string;
  if_bw: number;
  pitch?: number;
  enabled: boolean;
  processing_enabled?: boolean;
  baseband?: { passband_hz: number } | null;
}

let active: SdrChannel = 1;
let channels: SdrChannelMark[] = [];
let receiverNumbers: number[] | null = null;
export const currentSdrChannel = (): SdrChannel => active;
export const sdrChannelMarks = (): readonly SdrChannelMark[] => channels.filter(c => receiverNumbers?.includes(c.channel) ?? c.enabled);
export const knownSdrChannelNumbers = (): SdrChannel[] => [...new Set([...channels.map(c => c.channel), ...(receiverNumbers ?? []), active])];
export const receivingSdrChannels = (): readonly SdrChannelMark[] => channels.filter(c => c.enabled && c.processing_enabled !== false);

/** The native service distinguishes stopped definitions from unused compatibility slots. */
export function confirmSdrReceiverNumbers(numbers: number[] | null): void {
  if (JSON.stringify(numbers) === JSON.stringify(receiverNumbers)) return;
  receiverNumbers = numbers; requestRender();
}

/** Selection is local UI state. Changing it never retunes another receiver. */
export function setCurrentSdrChannel(channel: SdrChannel): void { if (isChannelNumber(channel)) active = channel; }
export function confirmSdrChannels(next: SdrChannelMark[]): void {
  const marks = next.filter(c => isChannelNumber(c.channel) && Number.isFinite(c.freq) && c.freq > 0)
    .map(({ channel, freq, mode, if_bw, pitch, enabled, processing_enabled, baseband }) =>
      ({ channel, freq, mode, if_bw, pitch, enabled, processing_enabled, baseband }));
  if (JSON.stringify(marks) !== JSON.stringify(channels)) { channels = marks; requestRender(); }
}
