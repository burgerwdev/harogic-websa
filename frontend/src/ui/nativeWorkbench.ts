import './workbench.css';
import { NativeReceiverAdapter } from '../core/nativeReceiverAdapter';
import { WorkbenchSession } from './workbenchSession';
import { mountWorkbench } from './workbenchApp';

let opened: ReturnType<typeof mountWorkbench> | null = null;
let pending: Promise<void> | null = null;

/** One control/data owner for every view in this page, including Pop out documents. */
export async function openNativeWorkbench(): Promise<void> {
  if (opened) { opened.recall(); return; }
  if (pending) return pending;
  pending = (async () => {
    const adapter = await NativeReceiverAdapter.connect();
    const session = new WorkbenchSession(adapter);
    const host = document.createElement('div'); host.hidden = true; document.body.append(host);
    opened = mountWorkbench(session, host);
    opened.recall();
    window.addEventListener('pagehide', () => { opened?.dispose(); session.dispose(); adapter.dispose(); host.remove(); }, { once: true });
  })();
  try { await pending; } finally { pending = null; }
}
