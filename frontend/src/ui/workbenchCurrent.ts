import type { ReceiverState } from '../core/multichannel';

export interface WorkbenchCurrent {
  receiverUid(): string | null;
  save(): Promise<string | null>;
}

export function matchesCurrentFrequency(receiver: ReceiverState, frequency: number | null | undefined): boolean {
  return frequency != null && Number.isFinite(frequency) && frequency > 0
    && Math.abs(receiver.confirmed.baseband.frequency_hz - frequency) < .5;
}

export function representsCurrent(receiver: ReceiverState, current: ReceiverState): boolean {
  return receiver !== current && matchesCurrentFrequency(receiver, current.confirmed.baseband.frequency_hz)
    && receiver.confirmed.purpose === current.confirmed.purpose && receiver.state === 'running'
    && receiver.coverage?.state !== 'out_of_band' && !receiver.fault && receiver.processing_state !== 'failed'
    && (!receiver.reception || receiver.reception.state === 'receiving');
}

/** Current remains a live source even when a saved channel represents its frequency. */
export function workbenchChannels(receivers: readonly ReceiverState[], _frequency: number | null | undefined, currentUid?: string | null) {
  const current = receivers.find(receiver => receiver.receiver_uid === currentUid);
  const saved = receivers.filter(receiver => receiver !== current);
  const represented = !!current && saved.some(receiver => representsCurrent(receiver, current));
  return { current, saved, showCurrent: !!current && !represented };
}
