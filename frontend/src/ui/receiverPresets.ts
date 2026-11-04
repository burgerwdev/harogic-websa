import { clone, type ReceiverConfig } from '../core/multichannel';
import { getLang, onLangChange } from '../core/i18n';
import type { WorkbenchSession } from './workbenchSession';

type Settings = Pick<ReceiverConfig, 'purpose' | 'baseband' | 'processors'>;
interface Presets { revision: string; presets: { name: string; settings: Settings }[] }
const endpoint = '/api/sdr/receiver-presets';
const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;

/** This subtree stays mounted during status updates and moves with its owning window. */
export function mountReceiverPresets(root: HTMLElement, session: WorkbenchSession, uid: string,
  review: (config: ReceiverConfig) => void): () => void {
  root.innerHTML = `<summary></summary><div class="mc-fields">
    <label><span data-preset-name-label></span><input data-preset-name maxlength="128"></label>
    <label><span data-preset-list-label></span><select data-preset-list></select></label></div>
    <div class="mc-toolbar"><button type="button" data-preset-save></button><button type="button" data-preset-load></button>
    <button type="button" data-preset-delete></button><button type="button" data-preset-refresh></button></div>
    <p class="mc-clock" data-preset-status role="status"></p>`;
  const get = <T extends HTMLElement>(selector: string): T => root.querySelector<T>(selector)!;
  const name = get<HTMLInputElement>('[data-preset-name]'), list = get<HTMLSelectElement>('[data-preset-list]');
  const save = get<HTMLButtonElement>('[data-preset-save]'), load = get<HTMLButtonElement>('[data-preset-load]');
  const remove = get<HTMLButtonElement>('[data-preset-delete]'), refresh = get<HTMLButtonElement>('[data-preset-refresh]');
  const status = get('[data-preset-status]'), service = session.snapshot.service_id;
  let data: Presets | null = null, abort: AbortController | null = null, disposed = false;
  name.value = session.snapshot.receivers.find(r => r.receiver_uid === uid)?.confirmed.name ?? '';
  const receiver = () => service === session.snapshot.service_id ? session.snapshot.receivers.find(r => r.receiver_uid === uid) : undefined;
  const controls = (): void => {
    save.disabled = !!abort || !data || !name.value.trim() || !!session.edits.get(uid)?.pending;
    load.disabled = !!abort || !data || !list.value || !!session.edits.get(uid)?.pending || session.retiring(uid);
    remove.disabled = !!abort || !data || !list.value;
    refresh.disabled = !!abort;
  };
  const labels = (): void => {
    get('summary').textContent = l('Presets', '频点预设');
    get('[data-preset-name-label]').textContent = l('Preset name', '预设名称');
    get('[data-preset-list-label]').textContent = l('Saved presets', '已保存预设');
    save.textContent = l('Save confirmed settings', '保存已确认设置');
    load.textContent = l('Review preset', '预览预设'); remove.textContent = l('Delete preset', '删除预设'); refresh.textContent = l('Refresh', '刷新');
    save.title = l('Save confirmed frequency, IQ and processing settings on this server; drafts are excluded', '在服务器保存已确认的频点、IQ 和处理设置；不保存草稿');
    load.title = l('Preview on this channel, keeping its name and reception state; apply explicitly', '在此信道预览，保留名称及接收启停状态；确认后应用');
    remove.title = l('Delete the saved preset; reception continues', '删除保存的预设；接收继续');
    refresh.title = l('Reload presets from this server', '重新读取此服务器的预设');
    if (list.options[0]) list.options[0].textContent = l('Select preset', '选择预设');
  };
  const showList = (): void => {
    const selected = list.value;
    list.replaceChildren();
    for (const entry of [{ name: '' }, ...(data?.presets ?? [])]) {
      const option = root.ownerDocument.createElement('option'); option.value = entry.name; option.textContent = entry.name; list.append(option);
    }
    list.value = selected; labels(); controls();
  };
  const request = async (payload?: Record<string, unknown>): Promise<void> => {
    if (abort || !receiver()) return;
    const controller = new AbortController(); abort = controller; controls();
    status.textContent = l('Loading…', '读取中…');
    try {
      const response = await fetch(endpoint, { method: payload ? 'POST' : 'GET', cache: 'no-store',
        headers: payload ? { 'Content-Type': 'application/json' } : undefined,
        body: payload ? JSON.stringify({ ...payload, service_id: service, expected_revision: data?.revision }) : undefined,
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8000)]) });
      if (!response.ok) {
        const error = await response.json().catch(() => ({}));
        if (response.status === 409) data = null;
        throw new Error(error.error ?? `HTTP ${response.status}`);
      }
      const result = await response.json() as Presets;
      if (disposed || controller.signal.aborted || !receiver()) return;
      data = result; showList();
      if (payload?.type === 'save') list.value = String(payload.name);
      status.textContent = payload?.type === 'delete' ? l('Deleted', '已删除') : payload ? l('Saved', '已保存') : '';
    } catch (error) {
      if (!disposed && !controller.signal.aborted && receiver()) status.textContent = error instanceof Error ? error.message : String(error);
    } finally { if (!disposed) { abort = null; controls(); } }
  };
  const onToggle = (): void => { if ((root as HTMLDetailsElement).open && !data) void request(); };
  root.addEventListener('toggle', onToggle);
  name.addEventListener('input', controls);
  list.addEventListener('change', () => { if (list.value) name.value = list.value; controls(); });
  refresh.addEventListener('click', () => { void request(); });
  save.addEventListener('click', () => {
    const r = receiver(); if (!r || !data || session.edits.get(uid)?.pending) return;
    const presetName = name.value.trim();
    if (data.presets.some(p => p.name === presetName) && !root.ownerDocument.defaultView?.confirm(l(`Replace preset “${presetName}”?`, `替换预设“${presetName}”？`))) return;
    void request({ type: 'save', name: presetName, receiver_uid: uid, receiver_revision: r.revision });
  });
  load.addEventListener('click', () => {
    const r = receiver(), preset = data?.presets.find(p => p.name === list.value);
    if (r && preset && !session.edits.get(uid)?.pending && !session.retiring(uid)) review({ ...clone(r.confirmed),
      purpose: preset.settings.purpose, baseband: clone(preset.settings.baseband), processors: clone(preset.settings.processors) });
  });
  remove.addEventListener('click', () => {
    if (list.value && root.ownerDocument.defaultView?.confirm(l(`Delete preset “${list.value}”?`, `删除预设“${list.value}”？`)))
      void request({ type: 'delete', name: list.value });
  });
  const stopLang = onLangChange(labels), stopState = session.subscribe(controls); showList();
  if ((root as HTMLDetailsElement).open) void request();
  return () => { disposed = true; abort?.abort(); stopLang(); stopState(); root.removeEventListener('toggle', onToggle); };
}
