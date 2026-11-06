import type { AudioRecordingFormat, RecordingTask, TrackKind } from '../core/multichannel';
import { getLang, onLangChange } from '../core/i18n';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import { patchWorkbench } from './workbenchDom';
import { workbenchIcon as icon } from './workbenchIcons';
import { formatLabel, recordingEscape as esc, syncAudioFormatMenus, wireAudioFormatMenus } from './recordingFormats';
import type { WorkbenchSession } from './workbenchSession';
import { frequencyLabel } from './frequencyInput';
import { recordingFinished as finished, recordingStateLabel as stateLabel, recordingBytes as bytes, recordingSize as size, recordingDuration as duration, recordingDate, recordingFrequencies, recordingName, recordingChannel, recordingMatches, newestRecordingFirst } from './recordingPresentation';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
interface LibraryActions {
  archive(task: RecordingTask): string;
  openAudio(task: RecordingTask): void;
  openIq(task: RecordingTask): void;
  chooseFormat(task: string, format: AudioRecordingFormat): void;
}

export function mountRecordingLibrary(session: WorkbenchSession, actions: LibraryActions): { open(): void; confirmDelete(task: RecordingTask): void; recall(reset?: boolean): void; dispose(): void } {
  let surface: FloatingWindow | null = null, query = '', kind = 'all', state = 'all', editing: string | null = null, deleting: string | null = null;
  let draft = '', error = '', disposed = false, interacting = false;
  let notice = '', selectionService = session.snapshot.service_id, batchDeleting = false, batchProgress = 0;
  let batchConfirmation: { tasks: RecordingTask[]; service: string | undefined } | null = null;
  const selected = new Set<string>();
  let visibleTasks: RecordingTask[] = [];
  let gestureDocument: Document | null = null;
  let releaseTimer: ReturnType<typeof setTimeout> | undefined;
  let unwireFormats: (() => void) | undefined;
  const clearGesture = (): void => {
    gestureDocument?.removeEventListener('pointerup', release, true);
    gestureDocument?.removeEventListener('pointercancel', release, true);
    gestureDocument?.defaultView?.removeEventListener('blur', release);
    gestureDocument = null;
  };
  const release = (): void => {
    clearGesture();
    releaseTimer = setTimeout(() => { interacting = false; render(); }, 0);
  };
  const pending = new Set<string>();
  const can = (action: string): boolean => session.snapshot.transport === 'fixture' || !!session.snapshot.capabilities.commands?.includes(action);
  const taskName = (task: RecordingTask): string => recordingName(task, session.snapshot);
  const button = (action: string, label: string, options = ''): string => `<button type="button" data-file-action="${action}" title="${esc(label)}" ${options}>${esc(label)}</button>`;
  const clearSelection = (): void => { selected.clear(); batchConfirmation = null; notice = ''; };
  const deleteUnavailable = (task?: RecordingTask): string => !can('record_delete') ? l('Update and restart the server to enable file deletion', '更新并重启服务器后可删除文件')
    : task && !finished(task) ? l('Stop recording before deleting its files', '请先停止录制，再删除文件') : '';
  const render = (): void => {
    if (!surface || disposed || interacting) return;
    surface.sync();
    const tasks = session.snapshot.recordings;
    if (selectionService !== session.snapshot.service_id) { selectionService = session.snapshot.service_id; clearSelection(); editing = deleting = null; }
    for (const id of selected) if (!tasks.some(t => t.task_id === id && finished(t)) || !can('record_delete')) selected.delete(id);
    if (editing && !tasks.some(t => t.task_id === editing)) editing = null;
    if (deleting && !tasks.some(t => t.task_id === deleting)) deleting = null;
    const rows = tasks.filter(task => recordingMatches(task, query, session.snapshot)
      && (kind === 'all' || task.tracks.some(t => t.kind === kind))
      && (state === 'all' || state === 'active' && !finished(task) || state === task.state)).sort(newestRecordingFirst);
    visibleTasks = rows;
    const selectable = rows.filter(t => finished(t) && !pending.has(t.task_id) && can('record_delete'));
    const selectedTasks = tasks.filter(t => selected.has(t.task_id));
    const selectedSize = selectedTasks.reduce((sum, task) => sum + bytes(task), 0n);
    const allSelected = selectable.length > 0 && selectable.every(t => selected.has(t.task_id));
    const confirmed = batchConfirmation?.tasks ?? [];
    const focused = surface.body.ownerDocument.activeElement;
    patchWorkbench(surface.body, `<header class="mc-files-heading" data-key="heading"><div><h2>${l('Recording files', '录制文件')}</h2><p>${tasks.length} ${l('recordings', '项录制')}<span>·</span>${l('Total size', '总大小')} <strong data-files-total-size>${size(tasks.reduce((sum, t) => sum + bytes(t), 0n))}</strong>${tasks.some(t => !finished(t)) ? `<span>·</span>${tasks.filter(t => !finished(t)).length} ${l('active', '项进行中')}` : ''}</p></div>${icon('folder')}</header>
      <div class="mc-files-tools" data-key="tools"><label class="mc-search">${icon('search')}<input type="search" name="file-search" data-file-search value="${esc(query)}" placeholder="${l('Name, channel, 14M…', '名称、信道、14M…')}" aria-label="${l('Search recordings', '搜索录制文件')}" ${batchDeleting ? 'disabled' : ''}></label>
      <select name="file-state" data-file-state aria-label="${l('Recording status', '录制状态')}" ${batchDeleting ? 'disabled' : ''}>${[['all', l('All statuses', '全部状态')], ['active', l('In progress', '进行中')], ['completed', l('Completed', '已完成')], ['failed', l('Failed', '失败')]].map(([value, label]) => `<option value="${value}" ${value === state ? 'selected' : ''}>${label}</option>`).join('')}</select></div>
      <nav class="mc-files-filters" data-key="filters" aria-label="${l('Recording type', '录制类型')}">${[['all', l('All', '全部')], ['audio', l('Audio', '音频')], ['iq', 'IQ'], ['results', l('Results', '结果')]].map(([value, label]) => button('filter', label, `data-key="kind-${value}" data-kind="${value}" aria-pressed="${kind === value}" ${batchDeleting ? 'disabled' : ''}`)).join('')}<span>${rows.length} ${l('shown · newest first', '项 · 最新优先')}</span></nav>
      <div class="mc-files-selection" data-key="selection"><label title="${l('Select finished recordings in the current filtered list', '选择当前筛选列表中已结束的录制')}"><input type="checkbox" name="select-all-recordings" data-file-select-all ${allSelected ? 'checked' : ''} ${!selectable.length || batchDeleting ? 'disabled' : ''}><span>${l('Select all', '全选')}</span></label><span class="mc-files-selection-info" data-selected-info>${selectedTasks.length ? l(`${selectedTasks.length} selected · ${size(selectedSize)}`, `已选 ${selectedTasks.length} 项 · ${size(selectedSize)}`) : l('Select files to delete', '选择文件以批量删除')}</span>${selectedTasks.length ? button('clear-selection', l('Clear selection', '取消选择'), batchDeleting ? 'disabled' : '') : ''}${button('delete-selected', batchDeleting ? l(`Deleting ${batchProgress} / ${confirmed.length}`, `删除中 ${batchProgress} / ${confirmed.length}`) : l('Delete selected', '删除所选'), `class="mc-file-delete" ${!selectedTasks.length || batchDeleting || !can('record_delete') ? 'disabled' : ''}`)}</div>
      ${batchConfirmation ? `<div class="mc-files-batch-confirm" data-key="batch-confirm" role="group" aria-label="${l('Confirm batch deletion', '确认批量删除')}"><p>${l(`Delete ${confirmed.length} recordings (${size(confirmed.reduce((sum, t) => sum + bytes(t), 0n))})? This cannot be undone.`, `删除 ${confirmed.length} 项录制（${size(confirmed.reduce((sum, t) => sum + bytes(t), 0n))}）？此操作无法撤销。`)}</p><div>${button('cancel-batch-delete', l('Keep files', '保留文件'), batchDeleting ? 'disabled' : '')}${button('confirm-batch-delete', batchDeleting ? l('Deleting…', '删除中…') : l(`Delete ${confirmed.length} files`, `删除 ${confirmed.length} 项文件`), `class="mc-file-delete" ${batchDeleting ? 'disabled' : ''}`)}</div></div>` : ''}
      ${!can('record_delete') ? `<p class="mc-files-note" data-key="delete-unavailable">${deleteUnavailable()}</p>` : ''}
      ${session.snapshot.transport === 'fixture' ? `<p class="mc-files-note" data-key="demo">${l('Demo recordings · no files are written to disk.', '模拟录制 · 不写入实际文件。')}</p>` : ''}
      ${error ? `<p class="mc-error" role="alert" data-key="error">${esc(error)} ${button('dismiss', l('Dismiss', '关闭'))}</p>` : ''}
      ${notice ? `<p class="mc-files-note" data-key="notice" role="status">${esc(notice)}</p>` : ''}
      <div class="mc-files-list" data-key="list" aria-label="${l('Recording files', '录制文件')}">${rows.length ? rows.map(task => {
        const done = finished(task), busy = batchDeleting || pending.has(task.task_id);
        const track = task.tracks[0], type = track?.kind as TrackKind;
        const frequencies = recordingFrequencies(task);
        const date = recordingDate(task);
        return `<article class="mc-file${selected.has(task.task_id) ? ' mc-file-selected' : ''}" data-task-id="${esc(task.task_id)}" data-task-state="${task.state}"><div class="mc-file-main"><label class="mc-file-check" title="${esc(deleteUnavailable(task) || l('Select this recording', '选择此录制'))}"><input type="checkbox" data-key="file-checkbox" data-file-select="${esc(task.task_id)}" aria-label="${esc(l(`Select ${taskName(task)}`, `选择 ${taskName(task)}`))}" ${selected.has(task.task_id) ? 'checked' : ''} ${!done || busy || !can('record_delete') ? 'disabled' : ''}></label><span class="mc-file-icon">${icon(type === 'audio' ? 'volume' : type === 'iq' ? 'signal' : 'file')}</span><div class="mc-file-info">
        ${editing === task.task_id ? `<form data-file-rename><input name="recording-name" value="${esc(draft)}" required maxlength="128" aria-label="${l('Recording name', '录制名称')}" ${busy ? 'disabled' : ''}><button type="submit" title="${l('Save name', '保存名称')}" ${busy ? 'disabled' : ''}>${l('Save', '保存')}</button>${button('cancel-rename', l('Cancel', '取消'), busy ? 'disabled' : '')}</form>` : `<strong title="${esc(task.task_id)}">${esc(taskName(task))}</strong>`}
        <p><span>${type === 'audio' ? formatLabel(task.audio_format ?? 'wav') : type === 'iq' ? 'IQ' : 'JSONL'}</span>${duration(task) ? `<span>${duration(task)}</span>` : ''}${date ? `<time>${date}</time>` : ''}</p>
        <small>${esc(recordingChannel(task, session.snapshot))}${frequencies.length ? ` · ${esc(frequencies.slice(0, 2).map(frequencyLabel).join(' / '))}${frequencies.length > 2 ? ` +${frequencies.length - 2}` : ''}` : ''}</small></div><div class="mc-file-meta"><span class="mc-file-state" data-state="${task.state}"><i class="mc-state-dot" data-state="${done ? task.state : 'running'}"></i>${stateLabel(task.state)}</span><span class="mc-file-size" data-file-size title="${l('Stored recording size', '录制实际存储大小')}">${l('Size', '大小')} <b>${size(bytes(task))}</b></span></div></div>
        ${task.fault ? `<p class="mc-file-fault">${esc(task.fault.message)}</p>` : ''}
        <div class="mc-file-actions">${done ? actions.archive(task) : button('stop', task.state === 'finalizing' ? l('Saving…', '保存中…') : l('Stop recording', '停止录制'), busy || task.state === 'finalizing' ? 'disabled' : '')}<span class="mc-file-spacer"></span>${done && can('record_rename') ? button('rename', l('Rename', '重命名'), busy ? 'disabled' : '') : ''}<button type="button" data-file-action="delete" class="mc-file-delete" title="${esc(deleteUnavailable(task) || l('Delete this recording and its files', '删除此录制及其文件'))}" ${!done || busy || !can('record_delete') ? 'disabled' : ''}>${icon('trash')}<span>${l('Delete', '删除')}</span></button></div>
        ${deleting === task.task_id ? `<div class="mc-file-confirm" role="group" aria-label="${l('Confirm deletion', '确认删除')}"><span>${l('Delete this recording and its files? This cannot be undone.', '删除此录制及其文件？此操作无法撤销。')}</span>${button('cancel-delete', l('Keep', '保留'), busy ? 'disabled' : '')}${button('confirm-delete', busy ? l('Deleting…', '删除中…') : l('Delete files', '删除文件'), `class="mc-file-delete" ${busy ? 'disabled' : ''}`)}</div>` : ''}</article>`;
      }).join('') : `<div class="mc-workspace-empty mc-files-empty">${icon(tasks.length ? 'search' : 'folder')}<h2>${tasks.length ? l('No matching recordings', '没有匹配的录制') : l('Your recordings appear here', '录制文件将显示在这里')}</h2><p>${tasks.length ? l('Search a channel name or frequency, such as 14M or 14.2 MHz.', '可搜索信道名或频率，例如 14M、14.2 MHz。') : l('Start recording from a channel. Manage audio, IQ and decoded results here.', '在信道中开始录制，即可在此管理音频、IQ 和解码结果。')}</p>${tasks.length ? button('clear', l('Clear filters', '清除筛选')) : ''}</div>`}</div>`, element => element === focused && element.matches('input:not([type=checkbox]),select'));
    const allCheckbox = surface.body.querySelector<HTMLInputElement>('[data-file-select-all]')!;
    allCheckbox.checked = allSelected;
    allCheckbox.indeterminate = !allSelected && selectable.some(t => selected.has(t.task_id));
    surface.body.querySelectorAll<HTMLInputElement>('[data-file-select]').forEach(input => { input.checked = selected.has(input.dataset.fileSelect!); });
    syncAudioFormatMenus(surface.body);
  };
  const mutate = async (task: RecordingTask, action: 'stop' | 'rename' | 'delete'): Promise<void> => {
    if (batchDeleting || pending.has(task.task_id)) return;
    pending.add(task.task_id); error = ''; notice = ''; render();
    const reply = await session.send(action === 'rename' ? { type: 'record_rename', request_id: session.requestId(), task_id: task.task_id, name: draft.trim() }
      : { type: action === 'delete' ? 'record_delete' : 'record_stop', request_id: session.requestId(), task_id: task.task_id });
    if (disposed) return;
    pending.delete(task.task_id);
    if (reply.fault) error = reply.fault.message;
    else { if (action === 'rename') editing = null; if (action === 'delete') deleting = null; }
    render();
  };
  const deleteSelected = async (): Promise<void> => {
    const confirmation = batchConfirmation;
    if (!confirmation || batchDeleting || !can('record_delete') || confirmation.service !== session.snapshot.service_id) return;
    batchDeleting = true; batchProgress = 0; error = notice = ''; editing = deleting = null;
    const failures: string[] = [];
    let removed = 0;
    for (const task of confirmation.tasks) pending.add(task.task_id);
    render();
    // The server shares one archive reader lease with file management. Commit
    // sequentially so requests cannot contend with another delete in this batch.
    for (const task of confirmation.tasks) {
      if (disposed) break;
      if (confirmation.service !== session.snapshot.service_id) {
        failures.push(l('The server changed. Select files again.', '服务已变更，请重新选择文件。')); break;
      }
      const current = session.snapshot.recordings.find(t => t.task_id === task.task_id);
      if (!current) selected.delete(task.task_id);
      else if (!finished(current)) failures.push(l('A selected recording is still in progress.', '所选录制仍在进行中。'));
      else {
        const reply = await session.send({ type: 'record_delete', request_id: session.requestId(), task_id: task.task_id });
        if (disposed) break;
        if (reply.fault) failures.push(`${taskName(task)}: ${reply.fault.message}`);
        else { removed++; selected.delete(task.task_id); }
      }
      batchProgress++; render();
    }
    for (const task of confirmation.tasks) pending.delete(task.task_id);
    batchDeleting = false; batchConfirmation = null;
    if (disposed) return;
    if (failures.length) error = l(`${removed} deleted; ${failures.length} failed. Failed files remain selected. ${failures[0]}`,
      `已删除 ${removed} 项，${failures.length} 项失败；失败文件保留选中，可重试。${failures[0]}`);
    else notice = l(`Deleted ${removed} recordings.`, `已删除 ${removed} 项录制。`);
    render();
  };
  const open = (): void => {
    if (surface) { render(); surface.show(); if (surface.portal.isPoppedOut()) surface.portal.popout(); return; }
    surface = createFloatingWindow('recording-library', () => l('Recording files', '录制文件'), { x: 130, y: 100, w: 720, h: 650 });
    surface.body.classList.add('mc-files-body');
    unwireFormats = wireAudioFormatMenus(surface.body);
    surface.body.addEventListener('pointerdown', () => {
      clearGesture(); interacting = true;
      gestureDocument = surface!.body.ownerDocument;
      gestureDocument.addEventListener('pointerup', release, true);
      gestureDocument.addEventListener('pointercancel', release, true);
      gestureDocument.defaultView?.addEventListener('blur', release);
    }, true);
    surface.body.addEventListener('click', event => {
      clearGesture(); interacting = false;
      const element = event.target as HTMLElement, control = element.closest<HTMLElement>('[data-file-action],[data-action]');
      if (!control || control.matches(':disabled')) return;
      const task = session.snapshot.recordings.find(t => t.task_id === control.closest<HTMLElement>('[data-task-id]')?.dataset.taskId);
      const action = control.dataset.fileAction;
      if (control.dataset.action === 'choose-audio-format') {
        event.preventDefault(); const menu = control.closest('details')!; menu.open = false;
        actions.chooseFormat(control.dataset.formatTask!, control.dataset.format as AudioRecordingFormat);
        render(); menu.querySelector('summary')?.focus(); return;
      }
      if (control.dataset.action === 'audio-archive' && task) { actions.openAudio(task); return; }
      if (control.dataset.action === 'archive' && task) { actions.openIq(task); return; }
      if (!action) return;
      event.preventDefault();
      if (action === 'filter') { kind = control.dataset.kind!; clearSelection(); }
      if (action === 'clear') { query = ''; kind = state = 'all'; clearSelection(); }
      if (action === 'clear-selection') clearSelection();
      if (action === 'dismiss') error = '';
      if (action === 'cancel-rename') editing = null;
      if (action === 'cancel-delete') deleting = null;
      if (action === 'cancel-batch-delete') batchConfirmation = null;
      if (action === 'delete-selected') {
        const tasks = session.snapshot.recordings.filter(t => selected.has(t.task_id) && finished(t) && !pending.has(t.task_id));
        if (tasks.length && can('record_delete')) { batchConfirmation = { tasks, service: session.snapshot.service_id }; editing = deleting = null; }
      }
      if (action === 'confirm-batch-delete') { void deleteSelected(); return; }
      if (task) {
        if (action === 'rename') { editing = task.task_id; draft = taskName(task); deleting = null; }
        if (action === 'delete') { deleting = task.task_id; editing = null; }
        if (action === 'confirm-delete') { void mutate(task, 'delete'); return; }
        if (action === 'stop') { void mutate(task, 'stop'); return; }
      }
      render();
      if (action === 'rename') { const input = surface!.body.querySelector<HTMLInputElement>('[name=recording-name]'); input?.focus(); input?.select(); }
      if (action === 'delete') surface!.body.querySelector<HTMLButtonElement>('[data-file-action=cancel-delete]')?.focus();
      if (action === 'delete-selected') surface!.body.querySelector<HTMLButtonElement>('[data-file-action=cancel-batch-delete]')?.focus();
    });
    surface.body.addEventListener('input', event => {
      const input = event.target as HTMLInputElement;
      if (input.matches('[data-file-search]')) { query = input.value; clearSelection(); render(); }
      if (input.name === 'recording-name') draft = input.value;
    });
    surface.body.addEventListener('change', event => {
      const input = event.target as HTMLInputElement;
      if (input.matches('[data-file-state]')) { state = input.value; clearSelection(); render(); }
      if (input.matches('[data-file-select-all],[data-file-select]')) {
        if (batchDeleting || input.disabled || !can('record_delete')) return;
        batchConfirmation = null; notice = ''; editing = deleting = null;
        const tasks = input.matches('[data-file-select-all]') ? visibleTasks : session.snapshot.recordings.filter(t => t.task_id === input.dataset.fileSelect);
        for (const task of tasks) if (finished(task) && !pending.has(task.task_id)) {
          if (input.checked) selected.add(task.task_id); else selected.delete(task.task_id);
        }
        render();
      }
    });
    surface.body.addEventListener('submit', event => {
      if (!(event.target as HTMLElement).matches('[data-file-rename]')) return;
      event.preventDefault(); const task = session.snapshot.recordings.find(t => t.task_id === editing);
      if (!draft.trim()) { error = l('Enter a recording name.', '请输入录制名称。'); render(); return; }
      if (task) void mutate(task, 'rename');
    });
    surface.body.addEventListener('keydown', event => {
      if (event.key === 'Escape') { const menu = (event.target as HTMLElement).closest<HTMLDetailsElement>('[data-format-menu]'); if (menu?.open) { menu.open = false; menu.querySelector('summary')?.focus(); event.preventDefault(); event.stopPropagation(); } else if (!batchDeleting && (editing || deleting || batchConfirmation)) { editing = deleting = null; batchConfirmation = null; render(); event.preventDefault(); event.stopPropagation(); } }
    });
    render(); surface.show();
  };
  const unsubscribe = session.subscribe(render), stopLang = onLangChange(render);
  return { open, confirmDelete: task => {
    if (batchDeleting || pending.has(task.task_id) || deleteUnavailable(task)) return;
    selectionService = session.snapshot.service_id; clearSelection(); editing = null; deleting = task.task_id;
    query = task.task_id; kind = state = 'all'; error = ''; open();
    surface?.body.querySelector<HTMLButtonElement>('[data-file-action=cancel-delete]')?.focus();
  }, recall: reset => surface?.recall(reset), dispose: () => { disposed = true; clearGesture(); clearTimeout(releaseTimer); unwireFormats?.(); unsubscribe(); stopLang(); surface?.dispose(); } };
}
