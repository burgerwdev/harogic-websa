import { sharedReceiverModeMemory } from './receiverModeMemory';
import './workbench.css';
import { NativeReceiverAdapter } from '../core/nativeReceiverAdapter';
import { WorkbenchSession } from './workbenchSession';
import { mountNativeWorkbenchStreams } from './nativeWorkbenchStreams';
import { mountWorkbench } from './workbenchApp';
import { mountSdrQuickChannel } from './sdrQuickChannel';
import { confirmSdrReceiverNumbers, currentSdrChannel } from './sdrChannels';
import { sdrAudioOn, sdrListenHz } from './sdrState';

interface NativeWorkbenchOwner {
  quickChannel: ReturnType<typeof mountSdrQuickChannel>;
  showOverview(): void;
  showChannels(): void;
  dispose(): void;
}
let owner: NativeWorkbenchOwner | null = null;
let pending: Promise<NativeWorkbenchOwner | null> | null = null;
let generation = 0;

const retireOwner = (): void => {
  const previous = owner;
  owner = null; pending = null; generation++;
  window.removeEventListener('pagehide', retireOwner);
  previous?.dispose();
};

/** One control/data owner for every view in this page, including Pop out documents. */
async function ensureNativeWorkbench(): Promise<NativeWorkbenchOwner | null> {
  if (owner) return owner;
  if (pending) return pending;
  const version = generation;
  // Register before connecting: a delayed GET must not revive an owner after pagehide.
  window.addEventListener('pagehide', retireOwner);
  const operation = (async () => {
    const adapter = await NativeReceiverAdapter.connect();
    if (version !== generation) { adapter.dispose(); return null; }
    const session = new WorkbenchSession(adapter,sharedReceiverModeMemory);
    const syncReceivers = () => confirmSdrReceiverNumbers(session.snapshot.receivers.map(r => r.display_number), session.snapshot);
    const stopReceivers = session.subscribe(syncReceivers); syncReceivers();
    session.masterMuted = !sdrAudioOn.get();
    const current = session.snapshot.receivers.find(r => r.display_number === currentSdrChannel());
    if (current) {
      session.select(current.receiver_uid);
      session.routes.set(current.receiver_uid, { ...session.route(current.receiver_uid), enabled: true });
    }
    const host = document.createElement('div'); host.hidden = true; document.body.append(host);
    const streams = session.snapshot.capabilities.monitor_mixing ? mountNativeWorkbenchStreams(session) : undefined;
    let opened: ReturnType<typeof mountWorkbench> | null = null;
    const views = () => opened ??= mountWorkbench(session, host, streams, () => sdrListenHz.get(), {
      receiverUid: () => session.snapshot.receivers.find(r => r.display_number === 1)?.receiver_uid ?? null,
      save: () => quickChannel.add(),
    });
    const quickChannel = mountSdrQuickChannel(session, document.body, () => views().showChannels());
    const select = () => {
      const receiver = session.snapshot.receivers.find(r => r.display_number === currentSdrChannel());
      if (receiver) session.select(receiver.receiver_uid);
    };
    document.addEventListener('websa:sdr-channel-selection', select);
    owner = { quickChannel,
      showOverview: () => views().showOverview(),
      showChannels: () => views().showChannels(),
      dispose: () => {
        document.removeEventListener('websa:sdr-channel-selection', select); stopReceivers(); confirmSdrReceiverNumbers(null);
        quickChannel.dispose(); opened?.dispose(); streams?.dispose(); session.dispose(); adapter.dispose(); host.remove();
      },
    };
    return owner;
  })();
  pending = operation;
  try { return await operation; }
  catch (error) {
    if (version !== generation) return null;
    window.removeEventListener('pagehide', retireOwner); throw error;
  } finally { if (pending === operation) pending = null; }
}

export async function openNativeWorkbench(): Promise<void> {
  const current = await ensureNativeWorkbench();
  if (current && current === owner) current.showOverview();
}
export async function openNativeChannelOverview(): Promise<void> {
  const current = await ensureNativeWorkbench();
  if (current && current === owner) current.showChannels();
}

export async function addCurrentSdrChannel(): Promise<string | null> {
  const current = await ensureNativeWorkbench();
  return current && current === owner ? current.quickChannel.add() : null;
}

export async function focusNativeSdrChannel(): Promise<void> {
  const current = await ensureNativeWorkbench();
  if (current && current === owner) document.dispatchEvent(new CustomEvent('websa:sdr-channel-selection'));
}
