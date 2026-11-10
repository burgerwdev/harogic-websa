import { getLang, onLangChange } from '../core/i18n';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;

/** Confirm one observed set of UIDs. Later additions and replacement services are never cleared. */
export function mountWorkbenchClear(session: WorkbenchSession, currentUid: () => string | null) {
  const dialog = document.createElement('dialog');
  dialog.className = 'mc-create-dialog mc-clear-dialog'; dialog.dataset.clearChannelsDialog = '';
  dialog.innerHTML = '<form><header><strong></strong></header><p data-clear-description></p><footer><button type="button" data-clear-cancel></button><button type="submit" class="mc-danger" data-clear-confirm></button></footer></form>';
  let observed: { service?: string; lifecycle: number; uids: string[]; current: string | null } | undefined;
  let busy = false, disposed = false;
  const cancel = dialog.querySelector<HTMLButtonElement>('[data-clear-cancel]')!;
  const submit = dialog.querySelector<HTMLButtonElement>('[data-clear-confirm]')!;
  const valid = (): boolean => !!observed && observed.service === session.snapshot.service_id && observed.lifecycle === session.lifecycleVersion;
  const sync = (): void => {
    if (disposed || !observed) return;
    if (observed && !valid()) { observed = undefined; dialog.close(); }
    dialog.querySelector('strong')!.textContent = l('Clear all channels?', '清除所有信道？');
    const saved = observed?.uids.filter(uid => uid !== observed!.current).length ?? 0;
    dialog.querySelector('[data-clear-description]')!.textContent = l(
      `Delete ${saved} saved channels and stop Current reception and monitoring. Active recordings on these channels will end; saved recording files are kept. Channel deletion cannot be undone.`,
      `删除 ${saved} 个已保存信道，并停止 Current 的接收和监听。相关录制将结束，已保存的录制文件会保留。信道删除后无法撤销。`);
    cancel.textContent = l('Cancel', '取消'); submit.textContent = busy ? l('Clearing…', '正在清除…') : l('Clear all channels', '清除所有信道');
    cancel.disabled = busy; submit.disabled = busy || !valid();
  };
  cancel.addEventListener('click', () => { if (!busy) { observed = undefined; dialog.close(); } });
  dialog.addEventListener('cancel', event => { if (busy) event.preventDefault(); else observed = undefined; });
  dialog.addEventListener('keydown', event => event.stopPropagation());
  dialog.addEventListener('submit', event => {
    event.preventDefault(); if (busy || !valid()) { sync(); return; }
    const candidate = observed!; busy = true; sync();
    void session.clearChannels(candidate.uids, candidate.current).finally(() => {
      busy = false; if (disposed) return;
      observed = undefined; dialog.close();
    });
  });
  const stop = session.subscribe(sync), stopLang = onLangChange(sync);
  return {
    open(doc: Document): void {
      if (disposed || busy || session.clearingChannels) return;
      if (dialog.open) { dialog.focus(); return; }
      observed = { service: session.snapshot.service_id, lifecycle: session.lifecycleVersion,
        uids: session.snapshot.receivers.map(r => r.receiver_uid), current: currentUid() };
      doc.body.append(dialog); sync(); dialog.showModal(); cancel.focus();
    },
    dispose(): void { disposed = true; observed = undefined; stop(); stopLang(); dialog.remove(); },
  };
}
