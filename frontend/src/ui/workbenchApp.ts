import { clone, type ReceiverState, type ReceiverConfig, type ViewBinding, type SceneCommand, type TrackKind } from '../core/multichannel';
import { getLang, onLangChange } from '../core/i18n';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import { WorkbenchSession } from './workbenchSession';
import { renderDecoderEntries } from './decoderResults';
import { mountQuickListen } from './quickListen';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const esc = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const button = (action: string, text: string, extra = ''): string => `<button type="button" data-action="${action}" ${extra}>${esc(text)}</button>`;
const labels = () => ({ listen: l('Listen', '听音'), digital: l('Digital', '数字'), iq: l('IQ only', '仅 IQ') });
const taskLabel = (state: string): string => ({ preparing: l('Preparing', '准备'), waiting: l('Waiting', '等待'), recording: l('Recording', '记录中'),
  finalizing: l('Finalizing', '收尾'), completed: l('Completed', '完成'), failed: l('Failed', '失败') })[state] ?? state;
interface Pane { window: FloatingWindow; kind: 'overview' | 'detail' | 'results'; binding: ViewBinding; tab: string; renderedUid?: string; dirty?: boolean }

export function mountWorkbench(session: WorkbenchSession, host: HTMLElement): { dispose(): void; recall(reset?: boolean): void } {
  const fixture = session.snapshot.transport === 'fixture';
  const available = (command: string): boolean => fixture || !!session.snapshot.capabilities.commands?.includes(command);
  const applyCapabilities = (body: HTMLElement): void => {
    if (fixture) return;
    body.querySelectorAll<HTMLButtonElement>('[data-action]').forEach(button => {
      const action = button.dataset.action!;
      if ((action.startsWith('rec-') && !available('record_start')) || ['listen', 'solo', 'scene', 'preset-save', 'preset-load'].includes(action)) {
        button.disabled = true; button.title = l('Not available in this phase', '本阶段尚未开放');
      }
    });
    body.querySelectorAll<HTMLElement>('.mc-monitor,footer.mc-toolbar').forEach(element => { element.hidden = true; });
  };
  const panes = new Map<string, Pane>();
  const saveViews = (): void => { try { localStorage.setItem('websa-workbench-views-v1', JSON.stringify([...panes.values()].map(p => ({ kind: p.kind, binding: p.binding, tab: p.tab })))); } catch { /* session only */ } };
  let search = '', scene: SceneCommand | null = null;
  let preset: ReceiverConfig | null = null;
  const receiver = (uid: string) => session.snapshot.receivers.find(r => r.receiver_uid === uid);
  const send = (command: Parameters<WorkbenchSession['send']>[0]): void => { void session.send(command); };
  const taskSummary = (r: ReceiverState): string => session.snapshot.recordings.filter(t => t.receiver_uid === r.receiver_uid)
    .map(t => `${t.tracks.map(k => k.kind.toUpperCase()).join('+')}: ${taskLabel(t.state)}${t.fault ? ` · ${t.fault.code}` : ''}`).join(' / ') || '—';
  const rowHTML = (row: ReceiverState): string => `<article class="mc-row ${row.receiver_uid === session.selected ? 'mc-selected' : ''}" data-uid="${row.receiver_uid}" style="--channel:${row.confirmed.color}">
          ${button('select', `C${row.display_number} · ${row.confirmed.name}`)}<span>${(row.confirmed.baseband.frequency_hz / 1e6).toFixed(6)} MHz · ${labels()[row.confirmed.purpose]} · ${row.state}</span>
          <div>${row.confirmed.purpose === 'listen' ? button('listen', `${session.route(row.receiver_uid).enabled ? '✓ ' : ''}${l('Listen', '听')}`) + button('solo', 'Solo') : ''}
          ${button('rec-iq', 'IQ REC')}${row.confirmed.purpose === 'listen' ? button('rec-audio', 'Audio REC') : ''}${row.confirmed.purpose === 'digital' ? button('rec-results', 'Results REC') : ''}
          ${button('detail', l('Details', '详情'))}${button('results', 'Decode')}</div><small>${esc(taskSummary(row))}</small></article>`;
  const record = (uid: string, kind: TrackKind): void => {
    const task = session.snapshot.recordings.find(t => t.receiver_uid === uid && t.tracks.some(track => track.kind === kind) && ['preparing', 'recording', 'waiting'].includes(t.state));
    send(task ? { type: 'record_stop', request_id: session.requestId(), task_id: task.task_id }
      : { type: 'record_start', request_id: session.requestId(), receiver_uid: uid, kinds: [kind] });
  };
  const detail = (r: ReceiverState, pane: Pane): string => {
    const c = r.confirmed, edit = session.edits.get(r.receiver_uid), route = session.route(r.receiver_uid);
    const tabs = ['receive', 'processing', 'output', 'recording', 'results'];
    const tabNames = [l('Receive', '接收'), l('Processing', '处理'), l('Output', '输出'), l('Record', '记录'), l('Results', '结果')];
    let content = '';
    if (pane.tab === 'receive') content = `<form data-config><label>${l('Name', '名称')}<input name="name" value="${esc(c.name)}"></label>
      <label>${l('Frequency / Hz', '频率 / Hz')}<input name="frequency" type="number" step="100" value="${c.baseband.frequency_hz}" required></label>
      <label>${l('Full IQ passband / Hz', '完整 IQ 通带 / Hz')}<input name="bandwidth" type="number" min="1" value="${c.baseband.passband_hz}" required></label>
      <label>${l('Transition / Hz', '过渡带 / Hz')}<input name="transition" type="number" min="1" value="${c.baseband.transition_hz}" required></label>
      <label>${l('Minimum IQ rate / S/s', '最低 IQ 率 / S/s')}<input name="minimum-rate" type="number" min="1" value="${c.baseband.minimum_rate_hz}" required></label>
      <label>${l('Purpose', '用途')}<select name="purpose">${Object.entries(labels()).map(([v, label]) => `<option value="${v}" ${c.purpose === v ? 'selected' : ''}>${label}</option>`).join('')}</select></label>
      <p>${l('Draft → preflight → confirmed. Cancel or change target to discard.', '草稿 → 预检 → 确认；取消或切换目标撤销草稿。')}</p>
      <button type="submit">${l('Preview changes', '预览变更')}</button> ${button('cancel', l('Cancel draft', '撤销草稿'))}</form>
      <div data-preview></div>`;
    if (pane.tab === 'processing') content = `<p>${esc(c.processors.map(p => `${p.kind} · ${p.mode.toUpperCase()} · min ${p.minimum_rate_hz} S/s`).join(' / ') || l('No demodulator or decoder', '不创建解调器或解码器'))}</p>
      <p>${l('Baseband and processor settings have separate lifetimes.', '基带与处理分支分别管理。')}</p>`;
    if (pane.tab === 'output') content = `<p>CF32 LE · ${r.clock?.actual_rate_hz} S/s</p>${button('output', l('Toggle IQ subscription', '切换 IQ 订阅'))}
      <p>${esc(session.snapshot.outputs.filter(o => o.receiver_uid === r.receiver_uid).map(o => `${o.state} · ${o.samples ?? '0'} samples · ${o.bytes ?? '0'} bytes ${o.fault?.message ?? ''}`).join(' / ') || l('No consumers', '无消费者'))}</p>`;
    if (pane.tab === 'recording') content = !fixture ? `<p>${l('Persistent recording is not available yet.', '持久记录尚未开放。')}</p>` : `<p>${l('Fixture tasks only: no files are saved.', '仅模拟任务：不会保存文件。')}</p>
      ${button('rec-iq', l('Start / stop IQ', '开始 / 停止 IQ'))}
      ${c.purpose === 'listen' ? button('rec-audio', l('Start / stop analysis audio', '开始 / 停止分析音频')) : ''}
      ${c.purpose === 'digital' ? button('rec-results', l('Start / stop results', '开始 / 停止结果')) : ''}
      ${session.snapshot.recordings.filter(t => t.receiver_uid === r.receiver_uid).map(t => `<article class="mc-task" data-task-state="${t.state}"><b>${esc(t.task_id)} · ${taskLabel(t.state)}</b>
        <p>${esc(t.fault?.message ?? '')}</p>${t.tracks.map(track => `<p>${track.kind.toUpperCase()} · ${taskLabel(track.state)} · ${track.source} · ${track.compression} · ${track.segments.length} ${l('segments', '片段')}<br>${l('Persisted bytes', '已落盘字节')}: ${track.stored_bytes} (${l('fixture, no disk I/O', '模拟，无磁盘写入')})</p>`).join('')}</article>`).join('')}`;
    if (pane.tab === 'results') content = button('results', l('Open Decode view', '打开 Decode 视图'));
    return `<div class="mc-channel-heading" style="border-color:${esc(c.color)}"><b>C${r.display_number} · ${esc(c.name)}</b>
      <span>${labels()[c.purpose]} · ${r.state}</span>${button('toggle-receiver', c.enabled ? l('Stop receiver', '停用接收') : l('Enable receiver', '启用接收'))}</div>
      <p class="mc-clock">${(c.baseband.frequency_hz / 1e6).toFixed(6)} MHz · IQ ${r.clock?.actual_rate_hz} S/s · ${l('revision', '修订')} ${r.revision}
      <br>${esc(r.stream ? `${r.stream.source_id} / epoch ${r.stream.epoch} / channel ${r.stream.channel_id}` : l('No continuous stream', '当前无连续流'))}</p>
      ${r.fault ? `<p class="mc-error" role="alert">${esc(r.fault.scope)}: ${esc(r.fault.message)}</p>` : ''}
      ${edit?.pending ? `<p role="status">${l('Request pending', '请求中')} ${esc(edit.request_id)} → ${edit.desired.baseband.frequency_hz} Hz</p>` : ''}
      ${edit?.fault ? `<p class="mc-error" role="alert">${esc(edit.fault.code)}: ${esc(edit.fault.message)} ${button('retry', l('Retry', '重试'))}</p>` : ''}
      ${c.purpose === 'listen' ? `<fieldset class="mc-monitor"><legend>${l('Local monitor routing (simulated)', '本地监听路由（模拟）')}</legend>
        ${button('listen', `${route.enabled ? '✓ ' : ''}${l('Listen', '监听')}`)} ${button('solo', `${route.solo ? '✓ ' : ''}Solo`)}
        <label>${l('Volume', '音量')}<input data-gain type="range" min="0" max="1" step="0.01" value="${route.gain}"></label>
        <label>${l('Pan', '声像')}<input data-pan type="range" min="-1" max="1" step="0.1" value="${route.pan}"></label>
        <span>${session.audible(r.receiver_uid) ? l('Routed', '已路由') : l('Muted', '静音')}</span></fieldset>` : ''}
      <nav class="mc-tabs">${tabs.map((tab, i) => button(`tab-${tab}`, tabNames[i], `aria-pressed="${pane.tab === tab}"`)).join('')}</nav>${content}`;
  };
  function renderPane(pane: Pane): void {
    const uid = session.resolve(pane.binding), r = receiver(uid), body = pane.window.body;
    const focused = body.ownerDocument.activeElement as HTMLElement | null;
    // Incoming status must not replace a focused draft or a captured range pointer.
    if (pane.renderedUid === uid && ((pane.dirty && pane.tab === 'receive') || (focused && body.contains(focused) && focused.matches('input,select')))) {
      const list = body.querySelector('.mc-list');
      if (list) list.innerHTML = session.snapshot.receivers.filter(r => `${r.confirmed.name} ${r.confirmed.baseband.frequency_hz} ${r.confirmed.purpose} ${r.confirmed.tags.join(' ')}`.toLowerCase().includes(search.toLowerCase())).map(rowHTML).join('');
      applyCapabilities(body);
      return;
    }
    pane.renderedUid = uid; pane.dirty = false;
    if (pane.kind === 'overview') {
      const rows = session.snapshot.receivers.filter(r => `${r.confirmed.name} ${r.confirmed.baseband.frequency_hz} ${r.confirmed.purpose} ${r.confirmed.tags.join(' ')}`.toLowerCase().includes(search.toLowerCase()));
      body.innerHTML = `<p class="mc-notice">${fixture ? l('FIXTURE · no real reception, audio or recording', 'FIXTURE · 不进行真实接收、播放或记录')
        : session.snapshot.capture.source_kind === 'fixture' ? l('Native service · synthetic RF source', '原生服务 · 合成 RF 输入') : l('Native receiver service', '原生接收服务')} · ${session.snapshot.receivers.length}/${session.snapshot.capabilities.max_channels}</p>
        ${session.fault ? `<p class="mc-error" role="alert">${esc(session.fault.code)}: ${esc(session.fault.message)}</p>` : ''}
        <div class="mc-toolbar"><input data-search placeholder="${l('Search name / Hz / purpose / tags', '搜索名称 / Hz / 用途 / 标签')}" value="${esc(search)}">
        ${button('create', l('Add', '新增'))} ${button('preset-save', l('Save preset', '保存预设'))} ${button('preset-load', l('Load preset', '载入预设'), preset ? '' : 'disabled')}
        ${button('duplicate', l('Duplicate', '复制'))} ${button('retire', l('Retire', '删除定义'))} ${button('scene', l('Scene preview', '场景预览'))}</div>
        <div class="mc-split"><div class="mc-list">${rows.map(rowHTML).join('')}</div>
        <div class="mc-editor" data-editor>${r ? detail(r, pane) : l('No receiver selected', '无选中信道')}</div></div>
        <footer class="mc-toolbar"><label>${l('Monitor rule', '监听规则')} <select data-monitor-mode><option value="manual">${l('Manual mix', '手动混听')}</option><option value="follow">${l('Follow selection', '跟随选择')}</option></select></label>
        <label>${l('Master volume', '主音量')} <input data-master type="range" min="0" max="1" step="0.01" value="${session.masterGain}"></label>
        ${button('follow-results', l('Follow Decode', '跟随 Decode'))}</footer><div data-scene></div>`;
      body.querySelector<HTMLSelectElement>('[data-monitor-mode]')!.value = session.monitorMode;
      if (scene) showScene(pane);
    } else if (pane.kind === 'detail') body.innerHTML = r ? detail(r, pane) : l('Receiver retired; retained recordings remain available.', '信道已删除；已有记录历史保留。');
    else {
      body.innerHTML = `<label>${l('Binding', '绑定')} <select data-binding><option value="follow">${l('Follow selection', '跟随选择')}</option>
        ${session.snapshot.receivers.map(row => `<option value="${row.receiver_uid}">C${row.display_number} · ${esc(row.confirmed.name)}</option>`).join('')}</select></label>
        <p>${esc(r ? `C${r.display_number} · ${r.confirmed.name} · ${r.state}` : '—')} · ${fixture ? l('Fixture results', '模拟结果') : l('Results view will be connected in P4', '结果视图将在 P4 接入')}</p><div data-results></div>`;
      body.querySelector<HTMLSelectElement>('[data-binding]')!.value = pane.binding.type === 'follow' ? 'follow' : pane.binding.receiver_uid;
      const target = body.querySelector<HTMLElement>('[data-results]')!;
      renderDecoderEntries(target, fixture && r?.confirmed.purpose === 'digital' ? [{ id: 1, at: 1_793_404_800_000, source: r.receiver_uid,
        provider: 'fixture', mode: 'ft8', kind: 'table', bytes: 128,
        data: { decodes: [{ snr_db: -12, audio_hz: 1099, msg: 'FIXTURE CQ TEST AA00' }] } }] : [], uid, false);
    }
    pane.window.root.dataset.receiverUid = uid;
    applyCapabilities(body);
  }
  function showScene(pane: Pane): void {
    if (!scene) return;
    const impact = session.adapter.preflight(scene), target = pane.window.body.querySelector('[data-scene]');
    if (target) target.innerHTML = `<div class="mc-preview"><b>${l('Atomic scene preflight', '场景原子预检')}</b><p>${l('Shared center', '共享中心')}: ${session.snapshot.capture.center_hz} → ${scene.capture_center_hz} Hz</p>
      <p>${scene.configs.map(change => `${esc(change.receiver_uid)}: ${receiver(change.receiver_uid)?.confirmed.baseband.frequency_hz} → ${change.config.baseband.frequency_hz} Hz`).join('<br>')}</p><p>${l('Receivers', '信道')}: ${impact.affected_receivers.join(', ')}<br>${l('Recording tracks / new segments', '记录轨道 / 新片段')}: ${esc(impact.affected_tracks.join(', ') || '—')}</p>
      <p>${esc(impact.faults.map(f => f.message).join('; '))} · CF32 ${impact.estimated_cf32_bytes_per_second} B/s (${l('fixture estimate', '模拟估计')})</p>
      ${button('scene-apply', l('Apply scene', '应用场景'), impact.accepted ? '' : 'disabled')}${button('scene-cancel', l('Cancel', '取消'))}</div>`;
  }
  function open(kind: Pane['kind'], binding: ViewBinding = { type: 'follow' }): void {
    const id = `${kind}-${binding.type === 'follow' ? 'follow' : binding.receiver_uid}`;
    const old = panes.get(id); if (old) { old.window.show(); return; }
    const index = panes.size;
    const title = (): string => kind === 'overview' ? fixture ? l('Channel workbench · FIXTURE', '信道工作台 · FIXTURE') : l('Channel workbench', '信道工作台')
      : `${kind === 'results' ? 'Decode' : l('Channel details', '信道详情')} · ${(panes.get(id)?.binding ?? binding).type === 'follow' ? l('Follow', '跟随') : session.resolve(panes.get(id)?.binding ?? binding)}`;
    const surface = createFloatingWindow(id, title, kind === 'overview' ? { x: 52, y: 138, w: 900, h: 580 }
      : { x: 170 + index * 35, y: 180 + index * 25, w: 500, h: 470 });
    const pane: Pane = { kind, binding, window: surface, tab: 'receive' }; panes.set(id, pane);
    surface.body.addEventListener('click', event => {
      const target = (event.target as HTMLElement).closest<HTMLElement>('[data-action]'); if (!target) return;
      if (target.tagName === 'BUTTON' && (target as HTMLButtonElement).type !== 'submit') event.preventDefault();
      const uid = target.closest<HTMLElement>('[data-uid]')?.dataset.uid ?? session.resolve(pane.binding), r = receiver(uid);
      const action = target.dataset.action!;
      if (action.startsWith('tab-')) { pane.tab = action.slice(4); saveViews(); renderPane(pane); }
      else if (action === 'select') session.select(uid);
      else if (action === 'detail') { session.select(uid); open('detail', { type: 'fixed', receiver_uid: uid }); }
      else if (action === 'results') open('results', { type: 'fixed', receiver_uid: uid });
      else if (action === 'follow-results') open('results');
      else if (action === 'toggle-receiver' && r) void session.configure(uid, { ...clone(r.confirmed), enabled: !r.confirmed.enabled });
      else if (action === 'listen') session.monitor(uid, { enabled: !session.route(uid).enabled });
      else if (action === 'solo') session.monitor(uid, { solo: !session.route(uid).solo });
      else if (action.startsWith('rec-')) record(uid, action.slice(4) as TrackKind);
      else if (action === 'output') send({ type: 'output', request_id: session.requestId(), receiver_uid: uid,
        enabled: !session.snapshot.outputs.some(o => o.receiver_uid === uid && ['active', 'starting'].includes(o.state)) });
      else if (action === 'create' || action === 'duplicate') {
        if (r && (fixture || action === 'duplicate')) send({ type: 'create', request_id: session.requestId(), config: { ...clone(r.confirmed), name: `${r.confirmed.name} copy` } });
        else if (!fixture) send({ type: 'create', request_id: session.requestId(), config: {
          name: l('IQ receiver', 'IQ 信道'), color: '#4cc99b', tags: [], enabled: true, purpose: 'iq', processors: [],
          baseband: { frequency_hz: session.snapshot.capture.center_hz, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 },
        } });
      } else if (action === 'retire') send({ type: 'retire', request_id: session.requestId(), receiver_uid: uid });
      else if (action === 'preset-save' && r) { preset = clone(r.confirmed); session.notify(); }
      else if (action === 'preset-load' && preset) preview(pane, uid, clone(preset));
      else if (action === 'cancel') { pane.renderedUid = undefined; pane.dirty = false; renderPane(pane); }
      else if (action === 'retry' && r) void session.configure(uid, clone(session.edits.get(uid)?.attempted ?? r.confirmed));
      else if (action === 'scene') {
        scene = { type: 'scene', request_id: session.requestId(), expected_revision: session.snapshot.revision,
          capture_center_hz: session.snapshot.capture.center_hz + 10_000,
          configs: session.snapshot.receivers.map(r => ({ receiver_uid: r.receiver_uid, config: { ...clone(r.confirmed),
            baseband: { ...r.confirmed.baseband, frequency_hz: r.confirmed.baseband.frequency_hz + 1_000 } } })) };
        showScene(pane);
      } else if (action === 'scene-apply' && scene) { send(scene); scene = null; }
      else if (action === 'scene-cancel') { scene = null; renderPane(pane); }
    });
    surface.body.addEventListener('submit', event => {
      event.preventDefault(); const form = event.target as HTMLFormElement, uid = session.resolve(pane.binding), r = receiver(uid); if (!r) return;
      const data = new FormData(form), purpose = String(data.get('purpose')) as ReceiverConfig['purpose'];
      const config = clone(r.confirmed); config.name = String(data.get('name')); config.purpose = purpose;
      config.baseband = { ...config.baseband, frequency_hz: Number(data.get('frequency')),
        passband_hz: Number(data.get('bandwidth')), transition_hz: Number(data.get('transition')), minimum_rate_hz: Number(data.get('minimum-rate')) };
      if (purpose !== r.confirmed.purpose) config.processors = purpose === 'iq' ? [] : [{ id: `processor-${uid}`,
        kind: purpose === 'listen' ? 'demodulator' : 'decoder', mode: purpose === 'listen' ? 'nfm' : 'ft8', minimum_rate_hz: purpose === 'listen' ? 0 : 6_000 }];
      preview(pane, uid, config);
    });
    surface.body.addEventListener('input', event => {
      const input = event.target as HTMLInputElement, uid = session.resolve(pane.binding);
      if (input.closest('[data-config]')) pane.dirty = true;
      if (input.matches('[data-search]')) { search = input.value; input.blur(); renderPane(pane); const next = surface.body.querySelector<HTMLInputElement>('[data-search]')!; next.focus(); next.setSelectionRange(search.length, search.length); }
      if (input.matches('[data-gain]')) session.monitor(uid, { gain: Number(input.value) });
      if (input.matches('[data-pan]')) session.monitor(uid, { pan: Number(input.value) });
      if (input.matches('[data-master]')) { session.masterGain = Number(input.value); session.notify(); }
    });
    surface.body.addEventListener('focusout', event => {
      if (!(event.target as HTMLElement).matches('input[type=range]')) return;
      setTimeout(() => { if (surface.root.isConnected) renderPane(pane); }, 0);
    });
    surface.body.addEventListener('change', event => {
      const input = event.target as HTMLSelectElement;
      if (input.closest('[data-config]')) pane.dirty = true;
      if (input.matches('[data-monitor-mode]')) { session.monitorMode = input.value as 'manual' | 'follow'; input.blur(); session.notify(); }
      if (input.matches('[data-binding]')) { pane.binding = input.value === 'follow' ? { type: 'follow' } : { type: 'fixed', receiver_uid: input.value }; input.blur(); saveViews(); pane.window.sync(); renderPane(pane); }
    });
    renderPane(pane); saveViews();
  }
  function preview(pane: Pane, uid: string, config: ReceiverConfig): void {
    const r = receiver(uid); if (!r) return;
    const command = { type: 'configure' as const, request_id: session.requestId(), receiver_uid: uid, expected_revision: r.revision, config };
    const impact = session.adapter.preflight(command), target = pane.window.body.querySelector('[data-preview]'); if (!target) return;
    pane.dirty = true;
    target.innerHTML = `<div class="mc-preview"><p>${r.confirmed.baseband.frequency_hz} → ${config.baseband.frequency_hz} Hz · ${r.confirmed.purpose} → ${config.purpose}<br>
      ${l('Affected recording tracks', '受影响记录轨道')}: ${esc(impact.affected_tracks.join(', ') || '—')}<br>${esc(impact.faults.map(f => f.message).join('; '))}</p></div>`;
    const apply = target.ownerDocument.createElement('button'); apply.textContent = l('Apply to this receiver', '应用到此信道'); apply.disabled = !impact.accepted;
    apply.addEventListener('click', () => { pane.dirty = false; pane.renderedUid = undefined; void session.configure(uid, config); }); target.appendChild(apply);
  }
  const render = (): void => {
    for (const [action, text] of Object.entries({ workbench: l('Channel workbench', '信道工作台'), recall: l('Recall all windows', '召回全部窗口'), reset: l('Reset layout', '重置布局'), 'quick-listen': l('＋ Listen to frequency', '＋ 监听频点') })) {
      const control = host.querySelector<HTMLElement>(`[data-action="${action}"]`); if (control) control.textContent = text;
    }
    host.querySelector<HTMLElement>('[data-service-error]')!.textContent = session.fault ? `${session.fault.scope} · ${session.fault.code}: ${session.fault.message}` : '';
    host.querySelector<HTMLElement>('[data-capture]')!.textContent = `${session.snapshot.capture.center_hz / 1e6} MHz / ${session.snapshot.capture.usable_bandwidth_hz / 1000} kHz · ${session.snapshot.capture.state}`;
    const markers = host.querySelector<HTMLElement>('[data-markers]')!; markers.replaceChildren();
    session.snapshot.receivers.filter(r => r.confirmed.enabled).forEach(r => {
      const b = markers.ownerDocument.createElement('button'); b.dataset.markerUid = r.receiver_uid;
      b.textContent = `C${r.display_number} ${r.confirmed.name}${session.route(r.receiver_uid).enabled ? ` · ${l('Listen', '监听')}` : ''}`;
      b.style.left = `${50 + (r.confirmed.baseband.frequency_hz - session.snapshot.capture.center_hz) / session.snapshot.capture.usable_bandwidth_hz * 100}%`;
      b.style.top = `${r.display_number % 2 * 28 + 8}px`; b.style.borderColor = r.confirmed.color;
      b.addEventListener('click', () => session.select(r.receiver_uid)); markers.appendChild(b);
    });
    panes.forEach(pane => renderPane(pane));
  };
  const recall = (reset = false): void => panes.forEach(pane => pane.window.recall(reset));
  host.innerHTML = `<header class="mc-banner"><b>HAROGIC WebSA</b><span>${l('Multichannel development prototype', '多信道开发原型')} · FIXTURE</span></header>
    <main class="mc-desktop"><div class="mc-plots"><div class="mc-spectrum" tabindex="0" aria-label="Spectrum"><span>FIXTURE / dBFS · ${l('Right-click to add listening', '右键加入监听')}</span><div data-markers></div><div class="mc-trace"></div></div><div class="mc-waterfall" tabindex="0" aria-label="Waterfall"><span>${l('Illustrative waterfall · right-click to add listening', '示意瀑布 · 右键加入监听')}</span></div></div>
    <aside><b>${l('Shared capture', '共享采集')}</b><p data-capture></p>${button('quick-listen', l('＋ Listen to frequency', '＋ 监听频点'))}<p data-listen-status role="status"></p>${button('workbench', l('Channel workbench', '信道工作台'))}${button('recall', l('Recall all windows', '召回全部窗口'))}${button('reset', l('Reset layout', '重置布局'))}
    <p class="mc-notice">${l('No sockets, receivers, speakers or saved files. Refresh resets fixture tasks.', '不连接服务、设备、扬声器或文件。刷新会重置模拟任务。')}</p>
    <p data-service-error class="mc-error" role="alert"></p></aside></main>`;
  const quickListen = mountQuickListen(session, host);
  host.querySelectorAll<HTMLElement>('.mc-spectrum,.mc-waterfall').forEach(plot => {
    plot.addEventListener('keydown', event => {
      if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
        event.preventDefault(); quickListen.open(receiver(session.selected)?.confirmed.baseband.frequency_hz ?? session.snapshot.capture.center_hz);
      }
    });
    plot.addEventListener('contextmenu', event => {
      event.preventDefault();
      const marker = (event.target as HTMLElement).closest<HTMLElement>('[data-marker-uid]');
      const box = plot.getBoundingClientRect();
      const fromPoint = session.snapshot.capture.center_hz + (Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)) - 0.5) * session.snapshot.capture.usable_bandwidth_hz;
      quickListen.open(marker ? receiver(marker.dataset.markerUid!)!.confirmed.baseband.frequency_hz : Math.round(fromPoint));
    });
  });
  host.addEventListener('click', event => {
    const action = (event.target as HTMLElement).closest<HTMLElement>('[data-action]')?.dataset.action;
    if (action === 'quick-listen') quickListen.open(receiver(session.selected)?.confirmed.baseband.frequency_hz ?? session.snapshot.capture.center_hz);
    if (action === 'workbench') open('overview'); if (action === 'recall') recall(); if (action === 'reset') recall(true);
  });
  let storedViews: { kind: Pane['kind']; binding: ViewBinding; tab: string }[] = [];
  try { storedViews = JSON.parse(localStorage.getItem('websa-workbench-views-v1') ?? '[]'); } catch { /* invalid storage */ }
  open('overview');
  if (Array.isArray(storedViews)) for (const saved of storedViews.slice(0, 30)) {
    if (['detail', 'results'].includes(saved.kind) && saved.binding && (saved.binding.type === 'follow' || (saved.binding.type === 'fixed' && receiver(saved.binding.receiver_uid)))) { open(saved.kind, saved.binding); const pane = [...panes.values()][panes.size - 1]; if (['receive', 'processing', 'output', 'recording', 'results'].includes(saved.tab)) pane.tab = saved.tab; renderPane(pane); }
  }
  const unsubscribe = session.subscribe(render); const stopLang = onLangChange(() => { panes.forEach(p => p.window.sync()); render(); }); render();
  return { recall, dispose: () => { unsubscribe(); stopLang(); quickListen.dispose(); panes.forEach(p => p.window.dispose()); } };
}
