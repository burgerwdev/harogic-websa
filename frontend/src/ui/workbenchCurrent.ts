import type { ReceiverState } from '../core/multichannel';

export interface WorkbenchCurrent {
  receiverUid(): string | null;
  save(): Promise<string | null>;
}

export function matchesCurrentFrequency(receiver: ReceiverState, frequency: number | null | undefined): boolean {
  return frequency != null && Number.isFinite(frequency) && frequency > 0
    && Math.abs(receiver.confirmed.baseband.frequency_hz - frequency) < .5;
}

/** Current remains a live source even when a saved channel represents its frequency. */
export function workbenchChannels(receivers: readonly ReceiverState[], frequency: number | null | undefined, currentUid?: string | null) {
  const current = receivers.find(receiver => receiver.receiver_uid === currentUid);
  const saved = receivers.filter(receiver => receiver !== current);
  const represented = saved.some(receiver => matchesCurrentFrequency(receiver, frequency));
  return { current, saved, showCurrent: !!current && !represented };
}
