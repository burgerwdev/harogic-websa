import type { RecordingTask } from '../core/multichannel';
import { getLang, onLangChange } from '../core/i18n';
import type { WorkbenchSession } from './workbenchSession';
import { workbenchIcon } from './workbenchIcons';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;

export function recordingDeleteUnavailable(session: WorkbenchSession, task: RecordingTask): string {
  if (session.snapshot.transport !== 'fixture' && !session.snapshot.capabilities.commands?.includes('record_delete'))
    return l('Update and restart the server to enable file deletion', '更新并重启服务器后可删除文件');
  return ['completed', 'failed'].includes(task.state) ? '' : l('Stop recording before deleting its files', '请先停止录制，再删除文件');
}

/** A recording belongs to the service it was opened from, including recovered tasks. */
export function mountRecordingDelete(body: HTMLElement, session: WorkbenchSession, task: RecordingTask, removed: () => void): () => void {
  const service = session.snapshot.service_id;
  const root = body.ownerDocument.createElement('div');
  root.dataset.recordingDelete = '';
  root.innerHTML = `<div class="mc-toolbar"><button type="button" data-delete-recording class="mc-file-delete">${workbenchIcon('trash')}<span></span></button></div>
    <div data-delete-confirm class="mc-file-confirm" role="group" hidden><span></span><button type="button" data-delete-keep></button><button type="button" data-delete-confirm-button class="mc-file-delete"></button></div>
    <p data-delete-error class="mc-error" role="alert" hidden></p>`;
  body.append(root);
  const remove = root.querySelector<HTMLButtonElement>('[data-delete-recording]')!;
  const confirmation = root.querySelector<HTMLElement>('[data-delete-confirm]')!;
  const keep = root.querySelector<HTMLButtonElement>('[data-delete-keep]')!;
  const confirm = root.querySelector<HTMLButtonElement>('[data-delete-confirm-button]')!;
  const error = root.querySelector<HTMLElement>('[data-delete-error]')!;
  let pending = false, disposed = false;
  const current = (): RecordingTask | undefined => session.snapshot.recordings.find(t => t.task_id === task.task_id);
  const render = (): void => {
    if (disposed) return;
    const item = current();
    if (service !== session.snapshot.service_id || !item) { removed(); return; }
    const unavailable = recordingDeleteUnavailable(session, item);
    remove.querySelector('span')!.textContent = l('Delete recording', '删除录制');
    remove.title = unavailable || l('Delete this recording and all its files', '删除此录制及其全部文件');
    remove.disabled = pending || !!unavailable;
    confirmation.setAttribute('aria-label', l('Confirm deletion', '确认删除'));
    confirmation.querySelector('span')!.textContent = l('Delete this recording and all its files? This cannot be undone.', '删除此录制及其全部文件？此操作无法撤销。');
    keep.textContent = l('Keep', '保留'); keep.disabled = pending;
    confirm.textContent = pending ? l('Deleting…', '删除中…') : l('Delete files', '删除文件');
    confirm.disabled = pending || !!unavailable;
    if (unavailable) confirmation.hidden = true;
  };
  remove.addEventListener('click', () => { confirmation.hidden = false; error.hidden = true; render(); keep.focus(); });
  const cancel = (): void => { if (!pending) { confirmation.hidden = true; error.hidden = true; remove.focus(); } };
  keep.addEventListener('click', cancel);
  root.addEventListener('keydown', event => {
    if (event.key === 'Escape' && !confirmation.hidden) { event.preventDefault(); event.stopPropagation(); cancel(); }
  });
  confirm.addEventListener('click', () => {
    const item = current();
    if (pending || disposed || confirmation.hidden || service !== session.snapshot.service_id || !item || recordingDeleteUnavailable(session, item)) return;
    pending = true; error.hidden = true; render();
    void session.send({ type: 'record_delete', request_id: session.requestId(), task_id: task.task_id }).then(reply => {
      if (disposed) return;
      pending = false;
      if (!reply.fault) { removed(); return; }
      error.textContent = reply.fault.message; error.hidden = false; render();
    });
  });
  const unsubscribe = session.subscribe(render), stopLang = onLangChange(render);
  render();
  return () => { disposed = true; unsubscribe(); stopLang(); root.remove(); };
}
