import { setDomProperty } from './domState';
import { clone, type ReceiverConfig } from '../core/multichannel';
import { validModeControls } from '../core/receiverControls';
import { getLang, onLangChange } from '../core/i18n';
import type { WorkbenchSession } from './workbenchSession';
import { syncSelectOptions, syncSelectValue } from './selectControl';

type Settings = Pick<ReceiverConfig, 'purpose' | 'baseband' | 'processors'> & Partial<Pick<ReceiverConfig, 'continuity'>>;
interface Presets { version: 1; revision: string; presets: { name: string; settings: Settings }[] }
const endpoint = '/api/sdr/receiver-presets';
const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const validName = (value: unknown): value is string => typeof value === 'string' && !!value.trim() && value === value.trim()
  && new TextEncoder().encode(value).byteLength <= 128 && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
/** Validate the editor's data shape; mode availability, RF limits and budgets remain service admission. */
function readPresets(value: unknown): Presets {
  const invalid = (): never => { throw new Error(l('Invalid saved preset data; refresh after correcting the preset file.', '已保存预设数据无效；修正预设文件后请刷新。')); };
  if (!object(value) || value.version !== 1 || typeof value.revision !== 'string' || !/^\d{1,20}$/.test(value.revision)
    || BigInt(value.revision) > 18446744073709551615n || !Array.isArray(value.presets) || value.presets.length > 32) return invalid();
  const names = new Set<string>();
  for (const entry of value.presets) {
    if (!object(entry) || !validName(entry.name) || names.has(entry.name) || !object(entry.settings)) return invalid();
    names.add(entry.name);
    const settings = entry.settings, baseband = settings.baseband;
    if (typeof settings.purpose !== 'string' || !['iq', 'listen', 'digital'].includes(settings.purpose) || !object(baseband)
      || !['frequency_hz', 'passband_hz', 'transition_hz', 'minimum_rate_hz'].every(key => finite(baseband[key]) && baseband[key] > 0)
      || settings.continuity !== undefined && (typeof settings.continuity !== 'string' || !['continuous', 'strict'].includes(settings.continuity))
      || !Array.isArray(settings.processors)) return invalid();
    for (const processor of settings.processors) {
      if (!object(processor) || typeof processor.id !== 'string' || !processor.id || typeof processor.mode !== 'string' || !processor.mode
        || typeof processor.kind !== 'string' || !['demodulator', 'decoder'].includes(processor.kind) || !finite(processor.minimum_rate_hz) || processor.minimum_rate_hz < 0
        || !validModeControls(processor) || !['bandwidth_hz', 'pcm_gain', 'squelch_dbm', 'nr_strength', 'nr_atten_db'].every(key => processor[key] === undefined || finite(processor[key]))
        || !['agc', 'nr'].every(key => processor[key] === undefined || typeof processor[key] === 'boolean')
        || processor.nr_algorithm !== undefined && typeof processor.nr_algorithm !== 'string') return invalid();
    }
  }
  return value as unknown as Presets;
}

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
  let loadAttempted = false;
  let lifecycle = session.lifecycleVersion;
  const events = new AbortController();
  name.value = session.snapshot.receivers.find(r => r.receiver_uid === uid)?.confirmed.name ?? '';
  const receiver = () => !disposed && service === session.snapshot.service_id ? session.snapshot.receivers.find(r => r.receiver_uid === uid) : undefined;
  const controls = (): void => {
    const active = !!receiver();
    setDomProperty(save, 'disabled', !active || !!abort || !data || !validName(name.value.trim()) || !!session.edits.get(uid)?.pending);
    setDomProperty(load, 'disabled', !active || !!abort || !data || !list.value || !!session.edits.get(uid)?.pending || session.retiring(uid));
    setDomProperty(remove, 'disabled', !active || !!abort || !data || !list.value);
    setDomProperty(refresh, 'disabled', !active || !!abort);
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
  };
  const showList = (): void => {
    const selected = list.value;
    syncSelectOptions(list, [{ value: '', label: l('Select preset', '选择预设') },
      ...(data?.presets ?? []).map(entry => ({ value: entry.name, label: entry.name }))], selected);
    labels(); controls();
  };
  const request = async (payload?: Record<string, unknown>): Promise<void> => {
    if (abort || !receiver()) return;
    loadAttempted = true;
    const controller = new AbortController(); abort = controller; controls();
    const version = session.lifecycleVersion;
    const current = (): boolean => abort === controller && !controller.signal.aborted && !!receiver() && version === session.lifecycleVersion;
    status.textContent = l('Loading…', '读取中…');
    try {
      const response = await fetch(endpoint, { method: payload ? 'POST' : 'GET', cache: 'no-store',
        headers: payload ? { 'Content-Type': 'application/json' } : undefined,
        body: payload ? JSON.stringify({ ...payload, service_id: service, expected_revision: data?.revision }) : undefined,
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8000)]) });
      if (!current()) return;
      if (!response.ok) {
        const error = await response.json().catch(() => ({}));
        if (!current()) return;
        if (response.status === 409) data = null;
        throw new Error(error.error ?? `HTTP ${response.status}`);
      }
      const result: unknown = await response.json();
      if (!current()) return;
      data = null; data = readPresets(result); showList();
      if (payload?.type === 'save') syncSelectValue(list, String(payload.name));
      status.textContent = payload?.type === 'delete' ? l('Deleted', '已删除') : payload ? l('Saved', '已保存') : '';
    } catch (error) {
      if (current()) { if (!data) showList(); status.textContent = error instanceof Error ? error.message : String(error); }
    } finally { if (abort === controller) { abort = null; if (!disposed) controls(); } }
  };
  // An already-open details can deliver its queued toggle after a failed initial request.
  // Keep lazy loading separate from failure recovery, which requires explicit refresh.
  const onToggle = (): void => { if ((root as HTMLDetailsElement).open && !loadAttempted) void request(); };
  root.addEventListener('toggle', onToggle, { signal: events.signal });
  name.addEventListener('input', controls, { signal: events.signal });
  list.addEventListener('change', () => { if (list.value) name.value = list.value; controls(); }, { signal: events.signal });
  list.addEventListener('blur', showList, { signal: events.signal });
  refresh.addEventListener('click', () => { void request(); }, { signal: events.signal });
  save.addEventListener('click', () => {
    const r = receiver(); if (!r || !data || session.edits.get(uid)?.pending) return;
    const presetName = name.value.trim();
    if (!validName(presetName)) return;
    if (data.presets.some(p => p.name === presetName) && !root.ownerDocument.defaultView?.confirm(l(`Replace preset “${presetName}”?`, `替换预设“${presetName}”？`))) return;
    void request({ type: 'save', name: presetName, receiver_uid: uid, receiver_revision: r.revision });
  }, { signal: events.signal });
  load.addEventListener('click', () => {
    const r = receiver(), preset = data?.presets.find(p => p.name === list.value);
    if (r && preset && !session.edits.get(uid)?.pending && !session.retiring(uid)) review({ ...clone(r.confirmed),
      purpose: preset.settings.purpose, baseband: clone(preset.settings.baseband), processors: clone(preset.settings.processors),
      continuity: preset.settings.continuity ?? r.confirmed.continuity });
  }, { signal: events.signal });
  remove.addEventListener('click', () => {
    if (list.value && root.ownerDocument.defaultView?.confirm(l(`Delete preset “${list.value}”?`, `删除预设“${list.value}”？`)))
      void request({ type: 'delete', name: list.value });
  }, { signal: events.signal });
  const state = (): void => {
    if (disposed) return;
    if (!receiver() || lifecycle !== session.lifecycleVersion) {
      lifecycle = session.lifecycleVersion; abort?.abort(); abort = null; data = null;
      name.value = receiver()?.confirmed.name ?? ''; status.textContent = ''; showList();
    } else controls();
  };
  const stopLang = onLangChange(showList), stopState = session.subscribe(state); showList();
  if ((root as HTMLDetailsElement).open) void request();
  return () => { if (disposed) return; disposed = true; events.abort(); abort?.abort(); abort = null; data = null; stopLang(); stopState(); controls(); };
}
