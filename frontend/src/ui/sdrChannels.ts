import { isChannelNumber } from '../core/channelState';
import { requestRender } from '../render/redraw';

export type SdrChannel = number;
export interface SdrChannelActivity {
  channel: number; listening: boolean; audioRecording: boolean; iqRecording: boolean;
}
export interface SdrChannelControl {
  service_id: string; receiver_uid: string | null; revision: string; action_token?: string;
}
export interface SdrChannelMark {
  channel: SdrChannel;
  freq: number;
  mode: string;
  if_bw: number;
  pitch?: number;
  enabled: boolean;
  processing_enabled?: boolean;
  baseband?: { passband_hz: number } | null;
  control?: SdrChannelControl;
  activity?: SdrChannelActivity;
}

let active: SdrChannel = 1;
let channels: SdrChannelMark[] = [];
let receiverNumbers: number[] | null = null;
let activities: readonly SdrChannelActivity[] = [];
interface ReceiverDirectory {
  service_id?: string;
  action_token?: string;
  receivers: ReadonlyArray<{ display_number: number; receiver_uid: string; revision: string }>;
}
let receiverDirectory: { service: string; actionToken?: string;
  receivers: Map<number, { uid: string; revision: string }> } | null = null;
export const currentSdrChannel = (): SdrChannel => active;
export const sdrChannelMarks = (): readonly SdrChannelMark[] => channels.filter(c => receiverNumbers?.includes(c.channel) ?? c.enabled)
  .map(c => ({ ...c, activity: activities.find(a => a.channel === c.channel) }));
export const knownSdrChannelNumbers = (): SdrChannel[] => [...new Set([...channels.map(c => c.channel), ...(receiverNumbers ?? []), active])];
export const receivingSdrChannels = (): readonly SdrChannelMark[] => channels.filter(c => c.enabled && c.processing_enabled !== false);

/** The native service distinguishes stopped definitions from unused compatibility slots. */
export function confirmSdrReceiverNumbers(numbers: number[] | null, directory?: ReceiverDirectory, activity: readonly SdrChannelActivity[] = []): void {
  receiverDirectory = numbers !== null && directory?.service_id ? {
    service: directory.service_id, actionToken: directory.action_token,
    receivers: new Map(directory.receivers.map(r => [r.display_number, { uid: r.receiver_uid, revision: r.revision }])),
  } : null;
  const changed = JSON.stringify(numbers) !== JSON.stringify(receiverNumbers) || JSON.stringify(activities) !== JSON.stringify(activity);
  activities = activity;
  if (!changed) return;
  receiverNumbers = numbers; requestRender();
}

/** Selection is local UI state. Changing it never retunes another receiver. */
export function setCurrentSdrChannel(channel: SdrChannel): void { if (isChannelNumber(channel)) active = channel; }
export function confirmSdrChannels(next: SdrChannelMark[]): void {
  const marks = next.filter(c => isChannelNumber(c.channel) && Number.isFinite(c.freq) && c.freq > 0)
    .map(({ channel, freq, mode, if_bw, pitch, enabled, processing_enabled, baseband, control }) =>
      ({ channel, freq, mode, if_bw, pitch, enabled, processing_enabled, baseband,
        control: control ? { ...control } : undefined }));
  // Control revisions/tokens can rotate on an unchanged STATUS. Keep them
  // current for edits without repainting the spectrum and waterfall.
  const changed = marks.length !== channels.length || marks.some((mark, i) => {
    const previous = channels[i];
    return mark.channel !== previous.channel || mark.freq !== previous.freq || mark.mode !== previous.mode
      || mark.if_bw !== previous.if_bw || mark.pitch !== previous.pitch || mark.enabled !== previous.enabled
      || mark.processing_enabled !== previous.processing_enabled || mark.baseband?.passband_hz !== previous.baseband?.passband_hz;
  });
  channels = marks;
  if (changed) requestRender();
}

/** Freeze the identity observed when an edit is created, including unsent tuning. */
export function channelControlContext(channel = currentSdrChannel()): Record<string, unknown> {
  const control = channels.find(row => row.channel === channel)?.control;
  if (receiverDirectory) {
    const { service, actionToken, receivers } = receiverDirectory;
    const receiver = receivers.get(channel);
    // Null binds a new edit to an observed empty slot; the service rejects it if
    // another owner fills that slot before submission. It is not an omitted UID.
    if (!receiver) return { service_id: service, receiver_uid: null };
    const sameOwner = control?.service_id === service && control.receiver_uid === receiver.uid;
    // An acknowledged control can precede the adapter's next directory poll.
    const revision = sameOwner && typeof control.revision === 'string' && /^\d+$/.test(control.revision)
      && BigInt(control.revision) > BigInt(receiver.revision) ? control.revision : receiver.revision;
    return { service_id: service, receiver_uid: receiver.uid, expected_revision: revision,
      action_token: actionToken ?? (sameOwner ? control.action_token : undefined) };
  }
  return control?.receiver_uid ? { service_id: control.service_id, receiver_uid: control.receiver_uid,
    expected_revision: control.revision, action_token: control.action_token } : {};
}

/** Only our successful predecessor may advance a queued edit's revision; identity and action token stay frozen. */
export function advanceQueuedControl(command: Record<string, unknown>, predecessor: Record<string, unknown>,
  acknowledgement: SdrChannelControl | undefined): void {
  if (command.receiver_uid && acknowledgement?.receiver_uid === command.receiver_uid
      && acknowledgement.service_id === command.service_id
      && predecessor.receiver_uid === command.receiver_uid && predecessor.service_id === command.service_id
      && predecessor.expected_revision === command.expected_revision) {
    command.expected_revision = acknowledgement.revision;
  }
}
