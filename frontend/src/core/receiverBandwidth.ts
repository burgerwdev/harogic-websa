import type { ReceiverMode } from './multichannel';

/** Full symmetric IQ passband required by the processor, not its displayed search range.
 * Keep in sync with the native service_ddc::channel_plan geometry. */
export function occupiedBandwidth(mode: ReceiverMode, bandwidth = mode.defaultBandwidthHz, pitch = 700): number {
  if (mode.id === 'cw') return bandwidth + pitch * 2;
  if (mode.id === 'ft8') return Math.max(bandwidth, 3200) * 2;
  if (mode.id === 'sstv' || mode.id === 'radiofax') return Math.max(bandwidth, 3000) * 2;
  return ['upper', 'lower'].includes(mode.sideband) ? bandwidth * 2 : bandwidth;
}

/** Match the native mode planner's 12.5% transition for wide signals.
 * A fixed 1 kHz transition at MHz rates creates thousands of FIR taps. */
export function defaultTransitionHz(passband: number): number {
  return Math.max(1000, passband * 0.125);
}
