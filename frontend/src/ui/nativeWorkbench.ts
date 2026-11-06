import { sharedReceiverModeMemory } from './receiverModeMemory';
import './workbench.css';
import { NativeReceiverAdapter } from '../core/nativeReceiverAdapter';
import { WorkbenchSession } from './workbenchSession';
import { mountNativeWorkbenchStreams } from './nativeWorkbenchStreams';
import { mountWorkbench } from './workbenchApp';
import { mountSdrQuickChannel } from './sdrQuickChannel';
import { confirmSdrReceiverNumbers, currentSdrChannel } from './sdrChannels';
import { sdrAudioOn } from './sdrState';

let opened: ReturnType<typeof mountWorkbench> | null = null;
let pending: Promise<void> | null = null;
let quickChannel: ReturnType<typeof mountSdrQuickChannel> | null = null;
let mountViews: (() => ReturnType<typeof mountWorkbench>) | null = null;

/** One control/data owner for every view in this page, including Pop out documents. */
async function ensureNativeWorkbench(): Promise<void> {
  if (quickChannel) return;
  if (pending) return pending;
  pending = (async () => {
    const adapter = await NativeReceiverAdapter.connect();
    const session = new WorkbenchSession(adapter,sharedReceiverModeMemory);
    const syncReceivers = () => confirmSdrReceiverNumbers(session.snapshot.receivers.map(r => r.display_number));
    const stopReceivers = session.subscribe(syncReceivers); syncReceivers();
    session.masterMuted = !sdrAudioOn.get();
    const current = session.snapshot.receivers.find(r => r.display_number === currentSdrChannel());
    if (current) {
      session.select(current.receiver_uid);
      session.monitor(current.receiver_uid, { enabled: true });
    }
    const host = document.createElement('div'); host.hidden = true; document.body.append(host);
    const streams = session.snapshot.capabilities.monitor_mixing ? mountNativeWorkbenchStreams(session) : undefined;
    mountViews = () => mountWorkbench(session, host, streams);
    quickChannel = mountSdrQuickChannel(session, document.body);
    const select = () => {
      const receiver = session.snapshot.receivers.find(r => r.display_number === currentSdrChannel());
      if (receiver) session.select(receiver.receiver_uid);
    };
    document.addEventListener('websa:sdr-channel-selection', select);
    window.addEventListener('pagehide', () => { document.removeEventListener('websa:sdr-channel-selection', select); stopReceivers(); confirmSdrReceiverNumbers(null); quickChannel?.dispose(); opened?.dispose(); streams?.dispose(); session.dispose(); adapter.dispose(); host.remove(); }, { once: true });
  })();
  try { await pending; } finally { pending = null; }
}

export async function openNativeWorkbench(): Promise<void> {
  await ensureNativeWorkbench();
  opened ??= mountViews!();
  opened.showOverview();
}

export async function addCurrentSdrChannel(): Promise<string | null> {
  await ensureNativeWorkbench(); return quickChannel!.add();
}

export async function focusNativeSdrChannel(): Promise<void> {
  await ensureNativeWorkbench(); document.dispatchEvent(new CustomEvent('websa:sdr-channel-selection'));
}
