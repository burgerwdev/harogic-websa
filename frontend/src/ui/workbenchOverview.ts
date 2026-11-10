import { getLang } from '../core/i18n';
import { html, nothing, render as renderTemplate, type TemplateResult } from 'lit-html';
import { repeat } from 'lit-html/directives/repeat.js';
import { unsafeHTML } from 'lit-html/directives/unsafe-html.js';
import { live } from 'lit-html/directives/live.js';
import { getTheme } from '../core/theme';
import { getUiScale } from '../core/uiScale';
import { clone, receiverHasAudio, receiverInCapture, type ReceiverState, type RecordingTask, type TrackKind } from '../core/multichannel';
import type { WorkbenchSession } from './workbenchSession';
import type { ChannelStripReadouts } from './workbenchChannelStrip';
import { liveStatus } from './receiverLive';
import { ReceiverMeter, receiverLevel, receiverLevelScale } from './receiverLevel';
import { mountReceiverTicker, type ReceiverTicker } from './receiverTicker';
import { createWorkbenchPatcher } from './workbenchDom';
import { setDomAttribute, setDomProperty, setStyleProperty } from './domState';
import { workbenchIcon as icon } from './workbenchIcons';
import { matchesCurrentFrequency, workbenchChannels, type WorkbenchCurrent } from './workbenchCurrent';
import { cardGrid, cardSize, loadOverviewState, moveOverviewCard, receiverCreatedAt, reconcileOverview, saveOverviewState, snapOverviewPosition, sortOverview, vacantOverviewPosition, type CardPosition, type OverviewLayout, type OverviewSort } from './workbenchOverviewState';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const textModes = new Set(['ft8', 'ft4', 'wspr', 'cw', 'rtty', 'rtty-reverse', 'navtex', 'dsc']);
interface Options {
  available(command: string): boolean;
  recording(uid: string): { kind: TrackKind; task?: RecordingTask; pending: boolean } | undefined;
  readouts?: ChannelStripReadouts;
  selectedFrequency?(): number | null;
  current?: WorkbenchCurrent;
  detached(uid: string): boolean;
  dockAll(): void;
  fit(width: number, height: number): void;
  clearChannels?(document: Document): void;
}
interface CardView {
  root: HTMLElement;
  meter: ReceiverMeter;
  samples: Float32Array<ArrayBuffer>;
  ticker?: ReceiverTicker;
  probe: boolean;
  inView: boolean;
  theme?: string;
  color?: string;
}
export function createWorkbenchOverview(session: WorkbenchSession, options: Options) {
  const root = document.createElement('div'); root.className = 'mc-card-overview';
  const columnPitch = cardGrid.x, rowPitch = cardGrid.y;
  for (const [name, value] of Object.entries(cardSize)) root.style.setProperty(`--card-${name}`, `${value}px`);
  const controls = document.createElement('div'); controls.className = 'mc-overview-controls'; controls.dataset.noDrag = '';
  const state = loadOverviewState(session.snapshot.service_id ?? 'fixture');
  const views = new Map<string, CardView>();
  const patchControls = createWorkbenchPatcher(controls);
  const dates = new Map<string, { created: number | null; language: string; label: string; title: string; iso: string }>();
  const dateFormats = new Map<string, Intl.DateTimeFormat>();
  let arrangement = '';
  let cardVisibility: IntersectionObserver | null | undefined;
  let savedState = '';
  const events = new AbortController();
  let volume: string | undefined, stopGesture: (() => void) | undefined, lastPaint = 0, suppressClickUntil = 0, disposed = false;
  let rename: { uid: string; service: string; lifecycle: number; value: string; pending: boolean } | undefined;
  const recording = (uid: string): boolean => session.snapshot.recordings.some(task => task.receiver_uid === uid && ['preparing', 'waiting', 'recording', 'finalizing'].includes(task.state));
  const priority = (uid: string): number => Number(session.listening(uid)) * 4 + Number(recording(uid)) * 2 + Number(session.snapshot.receivers.find(r => r.receiver_uid === uid)?.state === 'running');
  const isCurrent = (uid: string): boolean => !!options.current && options.current.receiverUid() === uid;
  const ordered = (): ReceiverState[] => {
    const sorted = sortOverview(session.snapshot.receivers, state, priority), index = sorted.findIndex(r => isCurrent(r.receiver_uid));
    if (index > 0) sorted.unshift(...sorted.splice(index, 1));
    return sorted;
  };
  const shown = (): ReceiverState[] => {
    const all = ordered(), channels = workbenchChannels(all, options.selectedFrequency?.(), options.current?.receiverUid());
    return all.filter(r => isCurrent(r.receiver_uid) ? channels.showCurrent : !options.detached(r.receiver_uid)
      && (!state.bandOnly || receiverInCapture(session.snapshot, r.confirmed) !== false));
  };
  const glyph = (name: string) => unsafeHTML(icon(name));
  const cardButton = (action: string, symbol: string, label: string, disabled = false, pressed?: boolean, className = '') => html`<button type="button" data-action=${action} aria-label=${label} title=${label} ?disabled=${disabled} aria-pressed=${pressed === undefined ? nothing : String(pressed)} class=${className}>${glyph(symbol)}</button>`;
  const visible = (): boolean => root.isConnected && !root.closest<HTMLElement>('.mc-window')?.hidden && !root.closest<HTMLElement>('.mc-body')?.hidden && !root.ownerDocument.hidden;
  const releaseProbe = (uid: string, view: CardView): void => { if (view.probe) options.readouts?.waveform?.(uid, null); view.probe = false; };
  const disposeView = (uid: string, view: CardView): void => { cardVisibility?.unobserve(view.root); releaseProbe(uid, view); view.ticker?.dispose(); views.delete(uid); dates.delete(uid); };
  function positionNewCards(rows: ReceiverState[]): void {
    const columns = typeof state.layout === 'number' ? state.layout : Math.max(1, Math.floor((root.clientWidth - cardSize.padding * 2) / columnPitch));
    let slot = 0;
    for (const r of rows) {
      if (Object.prototype.hasOwnProperty.call(state.positions, r.receiver_uid)) continue;
      let point: { x: number; y: number };
      do {
        point = { x: slot % columns * columnPitch, y: Math.floor(slot / columns) * rowPitch }; slot++;
      } while (Object.values(state.positions).some(p => Math.abs(p.x - point.x) < cardSize.width && Math.abs(p.y - point.y) < cardSize.height));
      Object.defineProperty(state.positions, r.receiver_uid, { value: point, writable: true, enumerable: true, configurable: true });
    }
    if (state.layout === 'free' && state.snap) {
      const occupied: CardPosition[] = [];
      for (const r of rows) {
        if (options.detached(r.receiver_uid)) continue;
        const point = vacantOverviewPosition(state.positions[r.receiver_uid], occupied);
        state.positions[r.receiver_uid] = point; occupied.push(point);
      }
    }
  }
  function card(r: ReceiverState): TemplateResult {
    const uid = r.receiver_uid, c = r.confirmed, audio = receiverHasAudio(session.snapshot, c), temporary = isCurrent(uid);
    const route = session.route(uid), pan = route.pan, panLabel = pan < 0 ? 'L' : pan > 0 ? 'R' : 'L/R';
    const panTitle = l(`Audio output: ${panLabel}; click to cycle L/R → L → R`, `声像：${panLabel}；点击循环切换 L/R → L → R`);
    const monitor = audio && (session.snapshot.transport === 'fixture' || !!session.snapshot.capabilities.monitor_mixing);
    const record = options.recording(uid), task = record?.task;
    const availableAudio = r.state === 'running' && !r.fault;
    const created = receiverCreatedAt(r), language = getLang();
    let date = dates.get(uid);
    if (!date || date.created !== created || date.language !== language) {
      const locale = language === 'zh' ? 'zh-CN' : 'en-GB', value = created === null ? null : new Date(created);
      let format = dateFormats.get(language);
      if (!format) { format = new Intl.DateTimeFormat(locale, { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }); dateFormats.set(language, format); }
      date = { created, language, label: value ? format.format(value) : '—', iso: value?.toISOString() ?? '',
        title: `${l('Created', '创建')} ${value ? value.toLocaleString(locale) : l('Unknown', '未知')}` };
      dates.set(uid, date);
    }
    const point = state.positions[uid], position = state.layout === 'free' && point ? `;left:${point.x}px;top:${point.y}px` : '';
    const editing = rename?.uid === uid, busy = session.retiring(uid) || !!session.edits.get(uid)?.pending;
    const renameLabel = l('Rename channel; Enter saves, Escape cancels', '改名：回车保存，Esc 取消');
    return html`<article class="mc-overview-card" data-uid=${uid} data-tuning-source=${temporary} data-out-of-band=${r.coverage?.state === 'out_of_band'} data-key=${`card-${uid}`} data-listening=${monitor && session.listening(uid)} style=${`--channel:${c.color}${position}`} tabindex="0" aria-label=${`${temporary ? 'Current' : c.name} · ${(c.baseband.frequency_hz / 1e6).toFixed(6)} MHz`}>
      <header class="mc-card-heading"><span class="mc-card-number">${temporary ? 'Current' : `C${r.display_number.toString().padStart(2, '0')}`}</span>${temporary ? html`<strong class="mc-card-temporary-name">${l('Temporary tuning', '临时调谐')}</strong>` : editing ? html`<input data-card-name-edit data-key=${`name-${uid}`} aria-label=${renameLabel} .value=${live(rename!.value)} maxlength="128" ?readonly=${rename!.pending}>` : html`<button type="button" class="mc-card-name" data-card-rename=${uid} title=${`${c.name} · ${renameLabel}`} aria-label=${`${c.name} · ${renameLabel}`} ?disabled=${!options.available('configure') || busy}><span>${c.name}</span>${glyph('edit')}</button>`}${r.coverage?.state === 'out_of_band' ? html`<span class="mc-card-band-status" title=${liveStatus(r).label}>${l('Out of band', '带外')}</span>` : nothing}<span data-card-current hidden title=${l('Matches Current frequency', '与 Current 调谐频率一致')}>${glyph('crosshair')}</span><i class="mc-state-dot" data-state=${r.state} title=${liveStatus(r).label}></i></header>
      <div class="mc-card-frequency">${(c.baseband.frequency_hz / 1e6).toFixed(6)}<small>MHz</small></div>
      <div class="mc-card-meta"><span class="mc-card-mode">${c.processors[0]?.mode.toUpperCase() ?? 'IQ'}</span><span>BW ${((c.processors[0]?.bandwidth_hz ?? c.baseband.passband_hz) / 1000).toLocaleString(language, { maximumFractionDigits: 2 })} kHz</span><span class="mc-card-created" title=${date.title}>${glyph('clock')}<time aria-label=${date.title} datetime=${date.iso || nothing}>${date.label}</time></span></div>
      <div class="mc-card-visual" data-card-visual><div data-card-message></div><canvas data-card-wave width="256" height="32" hidden aria-label=${l('Audio waveform before monitor volume; normalized display', '监听音量之前的声音波形；幅度归一化显示')}></canvas><span data-card-placeholder></span></div>
      <div class="mc-card-meter" data-card-meter role="meter" aria-label=${l('Channel level', '信道电平')} aria-valuemin="-60" aria-valuemax="0"><span data-card-source>AF</span><div class="mc-card-meter-track"><i class="mc-strip-level-fill"></i><i class="mc-strip-level-peak"></i></div><small>— dBFS</small></div>
      <div class="mc-card-actions">${monitor ? cardButton('listen', 'headphones', session.listening(uid) ? l('Stop listening', '停止监听') : l('Listen', '监听'), !availableAudio, session.listening(uid)) : nothing}
      ${record && options.available('record_start') ? cardButton(`rec-${record.kind}`, 'record', task ? l('Stop recording', '停止录制') : l('Record channel', '录制信道'), record.pending || session.retiring(uid) || task?.state === 'finalizing' || !task && (session.snapshot.recording_resources?.available_tasks === 0 || r.state !== 'running'), !!task, task ? 'mc-recording-on' : '') : nothing}
      ${monitor ? html`<div class="mc-card-volume"><button type="button" data-card-volume=${uid} aria-label=${l('Channel volume', '信道音量')} title=${l('Channel volume', '信道音量')} aria-expanded=${volume === uid}>${glyph(route.gain ? 'volume' : 'muted')}</button><div class="mc-card-volume-panel" ?hidden=${volume !== uid}><label>${l('Volume', '音量')}<input type="range" min="0" max="1" step="0.01" .value=${live(String(route.gain))} data-card-gain=${uid} aria-label=${l('Channel volume', '信道音量')}></label><output>${Math.round(route.gain * 100)}%</output></div></div>` : nothing}<span class="mc-card-spacer"></span>
      ${monitor ? html`<button type="button" class="mc-card-pan" data-card-pan=${uid} aria-label=${panTitle} title=${panTitle} ?disabled=${session.retiring(uid)}>${panLabel}</button>` : nothing}
      ${temporary ? cardButton('save-current', 'plus', l('Save current channel', '保存当前信道')) : options.available('retire') ? cardButton('retire', 'trash', l('Delete channel', '删除信道'), busy, undefined, 'mc-card-delete') : nothing}${cardButton('detail', 'external', temporary ? l('Current settings', 'Current 设置') : l('Take channel out of overview', '从概览取出信道'), false, undefined, 'mc-card-detach')}</div></article>`;
  }
  function update(): void {
    if (disposed || stopGesture || !root.isConnected) return;
    const previousService = state.service;
    reconcileOverview(state, session.snapshot.receivers, session.snapshot.service_id ?? 'fixture');
    if (previousService !== state.service) { for (const [uid, view] of views) disposeView(uid, view); volume = undefined; rename = undefined; }
    if (rename && (!session.snapshot.receivers.some(r => r.receiver_uid === rename!.uid) || rename.lifecycle !== session.lifecycleVersion)) rename = undefined;
    const all = ordered(), rows = shown(), channels = workbenchChannels(all, options.selectedFrequency?.(), options.current?.receiverUid());
    state.order = all.map(r => r.receiver_uid);
    if (channels.showCurrent && state.layout === 'free') state.positions[channels.current!.receiver_uid] = { x: 0, y: 0 };
    positionNewCards(rows);
    const detached = channels.saved.filter(r => options.detached(r.receiver_uid)).length;
    patchControls(`<select data-overview-sort aria-label="${l('Sort channels', '信道排序')}">${([['created', l('Created', '创建时间')], ['frequency', l('Frequency', '频率')], ['name', l('Name', '信道名称')], ['type', l('Type', '信道类型')], ['auto', l('Automatic', '自动排序')], ['manual', l('Manual', '手动排列')]] as const).map(([value, label]) => `<option value="${value}" ${state.sort === value ? 'selected' : ''}>${label}</option>`).join('')}</select>
      <button type="button" data-overview-direction ${['manual', 'auto'].includes(state.sort) ? 'disabled' : ''} title="${state.descending ? l('Sort ascending', '改为升序') : l('Sort descending', '改为降序')}" aria-label="${state.descending ? l('Sort ascending', '改为升序') : l('Sort descending', '改为降序')}">${icon(state.descending ? 'descending' : 'ascending')}</button>
      <select data-overview-layout aria-label="${l('Card layout', '卡片布局')}"><option value="auto" ${state.layout === 'auto' ? 'selected' : ''}>${l('Auto grid', '自动网格')}</option>${Array.from({ length: 8 }, (_, i) => `<option value="${i + 1}" ${state.layout === i + 1 ? 'selected' : ''}>${i + 1} ${l(i ? 'columns' : 'column', '列')}</option>`).join('')}<option value="free" ${state.layout === 'free' ? 'selected' : ''}>${l('Free layout', '自由摆放')}</option></select>
      <select data-overview-band aria-label="${l('Channel coverage', '信道频段筛选')}"><option value="all" ${state.bandOnly ? '' : 'selected'}>${l('All bands', '全部频段')}</option><option value="in-band" ${state.bandOnly ? 'selected' : ''}>${l('Current band only', '仅当前频段')}</option></select>
      <button type="button" data-overview-mute aria-pressed="${session.masterMuted}" title="${session.masterMuted ? l('Unmute all channels', '取消全部信道静音') : l('Mute all channels', '全部信道静音')}" aria-label="${l('Mute all channels', '全部信道静音')}">${icon(session.masterMuted ? 'muted' : 'volume')}<span>${l('Mute all', '全部静音')}</span></button>
      <button type="button" data-overview-fit title="${l('Fit window to cards', '按卡片布局适配窗口')}" aria-label="${l('Fit window to cards', '按卡片布局适配窗口')}">${icon('fit')}</button><button type="button" data-overview-collect ${detached ? '' : 'disabled'} title="${l('Return all channel windows to overview', '将全部独立信道窗口放回概览')}" aria-label="${l('Return all channel windows to overview', '将全部独立信道窗口放回概览')}">${icon('collect')}</button>${options.clearChannels ? `<button type="button" data-overview-clear class="mc-overview-clear" ${!all.length || session.clearingChannels || !options.available('retire') ? 'disabled' : ''} title="${l('Delete saved channels and stop Current; confirmation required', '删除已保存信道并停止 Current，操作前需确认')}">${icon('trash')}<span>${l('Clear all channels', '清除所有信道')}</span></button>` : ''}`);
    // Lit compiles the static template once. UID keys retain each card and its
    // independently owned canvas/ticker while bindings update only changed fields.
    renderTemplate(html`<div class="mc-card-summary"><span data-card-capacity title=${l('Defined channels / service capacity', '已建信道数／服务容量')}><i class="mc-state-dot" data-state="running"></i>${channels.current ? `${channels.saved.length} ${l('saved', '已保存')} · ` : ''}${all.length} / ${session.snapshot.capabilities.max_channels} ${l('channels', '信道')}</span><span>${glyph('headphones')}${all.filter(r => session.listening(r.receiver_uid) && receiverHasAudio(session.snapshot, r.confirmed)).length}</span><span class="mc-card-rec-count">${glyph('record')}${all.filter(r => recording(r.receiver_uid)).length}</span><span class="mc-card-summary-spacer"></span>${state.layout === 'free' ? html`<label><input type="checkbox" data-overview-snap .checked=${state.snap}>${l('Snap to grid', '网格吸附')}</label>` : html`<small>${l('Drag to arrange', '拖动卡片排列')}</small>`}</div>
      ${session.fault ? html`<p class="mc-error" role="alert">${session.fault.message}</p>` : nothing}<div class="mc-card-scroll"><div class=${`mc-card-grid${state.layout === 'free' ? ' mc-card-free' : ''}`} data-layout=${state.layout} style=${typeof state.layout === 'number' ? `--columns:${state.layout}` : ''}>${repeat(rows, r => `${state.service}:${r.receiver_uid}`, card)}${rows.length ? nothing : html`<div class="mc-card-empty">${glyph('channels')}<strong>${all.length ? l('Your channels are in separate windows', '信道已在独立窗口中') : l('Add a channel to start monitoring', '添加信道，开始监控')}</strong>${all.length ? html`<button type="button" data-overview-collect>${glyph('collect')}${l('Return all channels', '收回全部信道')}</button>` : html`<button type="button" data-action="create">${glyph('plus')}${l('Add channel', '添加信道')}</button>`}</div>`}</div></div>
      <footer class="mc-card-footer"><span data-overview-arrangement></span><span>${detached ? `${detached} ${l('in separate windows', '个独立窗口')}` : l('Double-click a card for details', '双击卡片查看详情')}</span></footer>`, root);
    for (const [uid, view] of views) if (!rows.some(r => r.receiver_uid === uid) || !root.contains(view.root)) disposeView(uid, view);
    const elements = new Map([...root.querySelectorAll<HTMLElement>('.mc-overview-card')].map(el => [el.dataset.uid, el]));
    if (cardVisibility === undefined) cardVisibility = typeof IntersectionObserver === 'undefined' ? null : new IntersectionObserver(entries => {
      for (const entry of entries) {
        const uid = (entry.target as HTMLElement).dataset.uid!, view = views.get(uid); if (!view || view.root !== entry.target) continue;
        view.inView = entry.isIntersecting; if (!view.inView) releaseProbe(uid, view);
      }
    }, { root: root.querySelector('.mc-card-scroll') });
    for (const r of rows) {
      const element = elements.get(r.receiver_uid)!;
      let view = views.get(r.receiver_uid);
      if (!view) { view = { root: element, meter: new ReceiverMeter(), samples: new Float32Array(256), probe: false, inView: true }; views.set(r.receiver_uid, view); cardVisibility?.observe(element); }
      const result = options.readouts?.results(r.receiver_uid);
      if (textModes.has(r.confirmed.processors[0]?.mode)) {
        const ownedView = view;
        view.ticker ??= mountReceiverTicker(element.querySelector<HTMLElement>('[data-card-message]')!, element, () => ownedView.inView);
        view.ticker.update(result?.entries ?? [], r, state.service, result?.source);
      } else if (view.ticker) { view.ticker.dispose(); view.ticker = undefined; }
    }
    const nextArrangement = `${getLang()}:${state.layout}:${rows.map(r => r.receiver_uid).join(':')}`;
    if (arrangement !== nextArrangement) { arrangement = nextArrangement; resizeCanvas(); }
    const nextState = JSON.stringify(state);
    if (savedState !== nextState) { saveOverviewState(state); savedState = nextState; }
    paint(true);
  }
  /** Decoder output updates the live text lane without rebuilding every card. */
  function updateResults(changed: ReadonlySet<string>): void {
    if (disposed || !visible()) return;
    for (const uid of changed) {
      const view = views.get(uid), receiver = session.snapshot.receivers.find(r => r.receiver_uid === uid);
      if (!view?.ticker || !receiver) continue;
      const result = options.readouts?.results(uid);
      view.ticker.update(result?.entries ?? [], receiver, state.service, result?.source);
    }
  }
  function resizeCanvas(): void {
    const grid = root.querySelector<HTMLElement>('.mc-card-grid'), scroll = root.querySelector<HTMLElement>('.mc-card-scroll');
    if (!grid || !scroll) return;
    if (state.layout === 'free') {
      const positions = [...views.keys()].map(uid => state.positions[uid]);
      grid.style.minWidth = `${Math.max(scroll.clientWidth - cardSize.padding * 2, ...positions.map(p => p.x + cardSize.width))}px`;
      grid.style.minHeight = `${Math.max(scroll.clientHeight - cardSize.padding * 2, ...positions.map(p => p.y + cardSize.height))}px`;
    }
    const label = root.querySelector<HTMLElement>('[data-overview-arrangement]')!;
    const columns = getComputedStyle(grid).gridTemplateColumns.split(' ').filter(value => value !== 'none').length || 1;
    const rows = Math.ceil(views.size / columns);
    label.textContent = state.layout === 'free' ? l('Free layout · drag to position', '自由摆放 · 拖动放置') : l(`${columns} ${columns === 1 ? 'column' : 'columns'} · ${rows} ${rows === 1 ? 'row' : 'rows'}`, `${columns} 列 · ${rows} 行`);
  }
  function paint(force = false): void {
    if (!force && performance.now() - lastPaint < 100) return;
    lastPaint = performance.now();
    const active = visible();
    if (!active) { for (const [uid, view] of views) releaseProbe(uid, view); return; }
    const viewport = cardVisibility ? undefined : root.querySelector<HTMLElement>('.mc-card-scroll')?.getBoundingClientRect();
    // Read all card geometry before changing meters/labels. Interleaving writes
    // and reads forces a separate layout flush for each receiver.
    const frames = [...views].map(([uid, view]) => ({ uid, view, rect: cardVisibility ? undefined : view.root.getBoundingClientRect() }));
    const frequency = options.selectedFrequency?.();
    for (const { uid, view, rect } of frames) {
      const r = session.snapshot.receivers.find(r => r.receiver_uid === uid); if (!r) continue;
      const matched = matchesCurrentFrequency(r, frequency);
      setDomProperty(view.root.dataset, 'frequencyMatch', String(matched));
      if (matched) setDomAttribute(view.root, 'aria-current', 'true'); else if (view.root.hasAttribute('aria-current')) view.root.removeAttribute('aria-current');
      setDomProperty(view.root.querySelector<HTMLElement>('[data-card-current]')!, 'hidden', !matched);
      if (!view.inView || viewport && rect && (rect.bottom < viewport.top || rect.top > viewport.bottom || rect.right < viewport.left || rect.left > viewport.right)) { releaseProbe(uid, view); continue; }
      const result = options.readouts?.results(uid);
      const hasAudio = receiverHasAudio(session.snapshot, r.confirmed);
      const reading = receiverLevel(r, result?.entries ?? [], options.readouts?.playback?.(uid), result?.source);
      const display = view.meter.update(reading, `${state.service}:${uid}:${r.stream?.epoch}:${r.stream?.channel_id}:${r.processing_runtime?.processing_generation}:${reading?.source}`);
      const scale = receiverLevelScale(reading ? { ...reading, db: display.level } : null), peak = receiverLevelScale(reading ? { ...reading, db: display.peak } : null);
      const meter = view.root.querySelector<HTMLElement>('[data-card-meter]')!;
      setStyleProperty(meter, '--level', `${scale.percent}%`); setStyleProperty(meter, '--peak', `${peak.percent}%`);
      setDomProperty(meter.dataset, 'available', String(!!reading)); setDomProperty(meter.dataset, 'clipped', String(display.clipped));
      setDomAttribute(meter, 'aria-valuemin', String(scale.min)); setDomAttribute(meter, 'aria-valuenow', String(scale.value));
      const sourceLabel = reading?.source === 'input' ? l('RF input', '射频输入') : reading?.source === 'playback' ? l('Playing audio before monitor volume', '播放音频，监听音量之前') : l('Demodulated audio, not playing', '解调音频，未播放');
      setDomProperty(meter, 'title', reading ? `${sourceLabel} · ${reading.db.toFixed(1)} dBFS` : l('No live level', '暂无实时电平'));
      setDomAttribute(meter, 'aria-valuetext', meter.title);
      setDomProperty(meter.querySelector('[data-card-source]')!, 'textContent', reading ? reading.source === 'input' ? 'RF' : 'AF' : hasAudio ? 'AF' : 'RF');
      setDomProperty(meter.querySelector('small')!, 'textContent', reading ? `${reading.db.toFixed(0)} dBFS` : '— dBFS');
      const canvas = view.root.querySelector<HTMLCanvasElement>('[data-card-wave]')!, placeholder = view.root.querySelector<HTMLElement>('[data-card-placeholder]')!;
      const message = view.root.querySelector<HTMLElement>('[data-card-message]')!;
      const text = !!view.ticker && !message.hidden;
      const audio = !text && r.coverage?.state !== 'out_of_band' && hasAudio && r.state === 'running' && !r.fault;
      const sampled = audio && !!options.readouts?.waveform?.(uid, view.samples);
      if (sampled) view.probe = true; else releaseProbe(uid, view);
      setDomProperty(canvas, 'hidden', !sampled); setDomProperty(placeholder, 'hidden', sampled || text);
      if (!sampled && !text) setDomProperty(placeholder, 'textContent', audio ? l('Receiving audio', '音频接收中') : liveStatus(r).label);
      if (!sampled) continue;
      const context = canvas.getContext('2d'); if (!context) continue;
      if (view.theme !== getTheme()) { view.theme = getTheme(); view.color = (canvas.ownerDocument.defaultView ?? window).getComputedStyle(canvas).getPropertyValue('--text').trim(); }
      let maximum = .02; for (const sample of view.samples) maximum = Math.max(maximum, Math.abs(sample));
      context.clearRect(0, 0, 256, 32); context.strokeStyle = view.color || '#72aa80'; context.lineWidth = 1;
      context.globalAlpha = .2; context.beginPath(); context.moveTo(0, 16); context.lineTo(256, 16); context.stroke();
      context.globalAlpha = .8; context.beginPath();
      for (let i = 0; i < 128; i++) { const y = 16 - view.samples[i * 2] / maximum * 13; if (i) context.lineTo(i * 2, y); else context.moveTo(0, y); }
      context.stroke();
    }
  }
  function fit(): void {
    const count = Math.max(1, views.size);
    const owner = root.ownerDocument.defaultView ?? window;
    const maxColumns = Math.max(1, Math.min(8, Math.floor((owner.innerWidth / getUiScale() - cardSize.padding * 2) / columnPitch)));
    const columns = typeof state.layout === 'number' ? state.layout : Math.min(maxColumns, Math.ceil(Math.sqrt(count * 1.5)));
    const positions = [...views.keys()].map(uid => state.positions[uid]);
    const width = state.layout === 'free' ? Math.max(350, ...positions.map(p => p.x + cardSize.width + cardSize.padding * 2 + 2)) : columns * columnPitch - cardSize.gap + cardSize.padding * 2 + 2;
    const height = (state.layout === 'free' ? Math.max(250, ...positions.map(p => p.y + cardSize.height + 128)) : Math.ceil(count / columns) * rowPitch + 120) + (Math.min(width, owner.innerWidth / getUiScale() - 24) <= 740 ? 32 : 0);
    options.fit(width, height);
  }
  function beginRename(uid: string): void {
    const receiver = session.snapshot.receivers.find(r => r.receiver_uid === uid);
    if (!receiver || !options.available('configure') || session.retiring(uid) || session.edits.get(uid)?.pending) return;
    rename = { uid, value: receiver.confirmed.name, service: state.service, lifecycle: session.lifecycleVersion, pending: false }; volume = undefined;
    update(); const field = root.querySelector<HTMLInputElement>('[data-card-name-edit]'); field?.focus(); field?.select();
  }
  function finishRename(save: boolean): void {
    const draft = rename; if (!draft || draft.pending) return;
    if (!save) { rename = undefined; update(); return; }
    const field = root.querySelector<HTMLInputElement>('[data-card-name-edit]'), name = draft.value.trim();
    if (!name || /[\u0000-\u001f\u007f]/.test(name)) {
      field?.setCustomValidity(l('Enter a channel name without control characters.', '请输入不含控制字符的信道名称。')); field?.reportValidity(); return;
    }
    if (new TextEncoder().encode(name).length > 128) { field?.setCustomValidity(l('Channel name is too long.', '信道名称过长。')); field?.reportValidity(); return; }
    const receiver = session.snapshot.receivers.find(r => r.receiver_uid === draft.uid);
    if (!receiver || state.service !== draft.service || session.lifecycleVersion !== draft.lifecycle) { rename = undefined; update(); return; }
    if (name === receiver.confirmed.name) { rename = undefined; update(); return; }
    if (session.retiring(draft.uid) || session.edits.get(draft.uid)?.pending) return;
    draft.pending = true; if (field) field.readOnly = true;
    void session.configure(draft.uid, { ...clone(receiver.confirmed), name }).then(() => {
      if (disposed || rename !== draft) return;
      const fault = session.edits.get(draft.uid)?.fault;
      if (fault && draft.service === state.service && draft.lifecycle === session.lifecycleVersion) {
        draft.pending = false; if (field?.isConnected) { field.readOnly = false; field.setCustomValidity(fault.message); field.reportValidity(); }
      } else rename = undefined;
      update();
    });
  }
  function click(event: MouseEvent): void {
    if (performance.now() < suppressClickUntil) { suppressClickUntil = 0; event.preventDefault(); event.stopImmediatePropagation(); return; }
    const target = event.target as HTMLElement;
    const name = target.closest<HTMLElement>('[data-card-rename]');
    if (name) { event.preventDefault(); event.stopPropagation(); beginRename(name.dataset.cardRename!); return; }
    if (target.closest('[data-overview-mute]')) {
      event.preventDefault(); event.stopPropagation(); session.masterMuted = !session.masterMuted; session.notify(); return;
    }
    const pan = target.closest<HTMLButtonElement>('[data-card-pan]');
    if (pan) {
      event.preventDefault(); event.stopPropagation(); const uid = pan.dataset.cardPan!;
      if (pan.disabled || !session.snapshot.receivers.some(r => r.receiver_uid === uid) || session.retiring(uid)) return;
      const current = session.route(uid).pan;
      session.monitor(uid, { pan: current === 0 ? -1 : current < 0 ? 1 : 0 }); return;
    }
    if (target.closest('[data-overview-clear]')) { event.preventDefault(); event.stopPropagation(); options.clearChannels?.(root.ownerDocument); return; }
    if (target.closest('[data-overview-fit]')) fit();
    if (target.closest('[data-overview-collect]')) options.dockAll();
    if (target.closest('[data-overview-direction]')) { state.descending = !state.descending; sortFree(); update(); }
    const button = target.closest<HTMLElement>('[data-card-volume]');
    if (button) { event.stopPropagation(); volume = volume === button.dataset.cardVolume ? undefined : button.dataset.cardVolume; update(); if (volume) root.querySelector<HTMLInputElement>('[data-card-volume-panel], .mc-card-volume-panel:not([hidden]) input')?.focus(); }
  }
  function sortFree(): void {
    if (state.layout !== 'free' || state.sort === 'manual') return;
    const columns = Math.max(1, Math.floor((root.clientWidth - cardSize.padding * 2) / columnPitch));
    shown().forEach((r, i) => state.positions[r.receiver_uid] = { x: i % columns * columnPitch, y: Math.floor(i / columns) * rowPitch });
  }
  function change(event: Event): void {
    const target = event.target as HTMLSelectElement;
    if (target.matches('[data-overview-band]')) { state.bandOnly = target.value === 'in-band'; update(); }
    if (target.matches('[data-overview-layout]')) {
      if (target.value === 'free') {
        root.querySelectorAll<HTMLElement>('.mc-overview-card').forEach(card => state.positions[card.dataset.uid!] = snapOverviewPosition({ x: card.offsetLeft, y: card.offsetTop }, state.snap)); state.sort = 'manual';
      }
      state.layout = target.value === 'auto' || target.value === 'free' ? target.value : Number(target.value) as OverviewLayout;
      update();
    }
    if (target.matches('[data-overview-sort]')) { state.sort = target.value as OverviewSort; sortFree(); update(); }
    if (target.matches('[data-overview-snap]')) {
      state.snap = (target as unknown as HTMLInputElement).checked;
      update();
    }
  }
  function input(event: Event): void {
    const target = event.target as HTMLInputElement;
    if (target.matches('[data-card-name-edit]') && rename && !rename.pending) { rename.value = target.value; target.setCustomValidity(''); return; }
    if (!target.matches('[data-card-gain]')) return;
    const uid = target.dataset.cardGain!;
    if (!session.snapshot.receivers.some(r => r.receiver_uid === uid) || session.retiring(uid)) return;
    target.closest('.mc-card-volume-panel')!.querySelector('output')!.textContent = `${Math.round(Number(target.value) * 100)}%`;
    session.monitor(uid, { gain: Number(target.value) });
  }
  function pointerDown(event: PointerEvent): void {
    if (event.button !== 0 || (event.target as HTMLElement).closest('button,input,select,label,a,.mc-ticker-viewport')) return;
    const card = (event.target as HTMLElement).closest<HTMLElement>('.mc-overview-card'); if (!card) return;
    if (isCurrent(card.dataset.uid!)) return;
    stopGesture?.();
    card.focus({ preventScroll: true });
    const uid = card.dataset.uid!, rect = card.getBoundingClientRect(), owner = root.ownerDocument.defaultView ?? window;
    const doc = root.ownerDocument, scale = rect.width / (card.offsetWidth || cardSize.width), start = { x: event.clientX, y: event.clientY }, offset = { x: event.clientX - rect.left, y: event.clientY - rect.top };
    const scroll = root.querySelector<HTMLElement>('.mc-card-scroll')!, grid = root.querySelector<HTMLElement>('.mc-card-grid')!;
    const original = { ...state.positions[uid] };
    const occupied = [...views.keys()].filter(id => id !== uid).map(id => state.positions[id]);
    const originalSort = state.sort;
    let ghost: HTMLElement | undefined, target: string | undefined, after = false, active = false;
    event.preventDefault();
    const startDrag = (): void => {
      if (active) return;
      active = true; card.classList.add('mc-card-drag-source'); card.dataset.dropLabel = l('Drop here', '释放到这里');
      grid.classList.toggle('mc-card-snap-active', state.layout === 'free' && state.snap);
      ghost = card.cloneNode(true) as HTMLElement; ghost.removeAttribute('data-uid'); ghost.removeAttribute('tabindex'); ghost.setAttribute('aria-hidden', 'true');
      ghost.className = 'mc-overview-card mc-card-drag-ghost';
      Object.assign(ghost.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` }); doc.body.append(ghost);
    };
    const hold = setTimeout(startDrag, 200);
    const move = (next: PointerEvent): void => {
      if (!active && Math.hypot(next.clientX - start.x, next.clientY - start.y) < 6) return;
      startDrag(); next.preventDefault();
      ghost!.style.left = `${next.clientX - offset.x}px`; ghost!.style.top = `${next.clientY - offset.y}px`;
      const bounds = scroll.getBoundingClientRect();
      if (next.clientY > bounds.bottom - 28) scroll.scrollTop += 16; if (next.clientY < bounds.top + 28) scroll.scrollTop -= 16;
      if (next.clientX > bounds.right - 28) scroll.scrollLeft += 16; if (next.clientX < bounds.left + 28) scroll.scrollLeft -= 16;
      if (state.layout === 'free') {
        const origin = grid.getBoundingClientRect();
        const desired = { x: (next.clientX - origin.left - offset.x) / scale, y: (next.clientY - origin.top - offset.y) / scale };
        const point = state.snap ? vacantOverviewPosition(desired, occupied) : snapOverviewPosition(desired, false);
        state.positions[uid] = point; card.style.left = `${point.x}px`; card.style.top = `${point.y}px`;
        resizeCanvas();
      } else {
        if (next.clientX < bounds.left || next.clientX > bounds.right || next.clientY < bounds.top || next.clientY > bounds.bottom) return;
        const placeholder = card.getBoundingClientRect();
        if (target && next.clientX >= placeholder.left && next.clientX <= placeholder.right && next.clientY >= placeholder.top && next.clientY <= placeholder.bottom) return;
        const candidates = [...grid.querySelectorAll<HTMLElement>('.mc-overview-card')].filter(el => el !== card && !isCurrent(el.dataset.uid!));
        const distance = (el: HTMLElement): number => {
          const r = el.getBoundingClientRect();
          return Math.hypot(Math.max(r.left - next.clientX, 0, next.clientX - r.right), Math.max(r.top - next.clientY, 0, next.clientY - r.bottom));
        };
        const hit = candidates.sort((a, b) => distance(a) - distance(b))[0];
        if (hit) {
          target = hit.dataset.uid;
          const r = hit.getBoundingClientRect(), columns = getComputedStyle(grid).gridTemplateColumns.split(' ').length;
          after = next.clientY > r.bottom || next.clientY >= r.top && (columns === 1 ? next.clientY > r.top + r.height / 2 : next.clientX > r.left + r.width / 2);
          const reference = after ? hit.nextSibling : hit;
          if (reference !== card) grid.insertBefore(card, reference);
        }
      }
    };
    const finish = (cancel = false): void => {
      clearTimeout(hold); owner.removeEventListener('pointermove', move); owner.removeEventListener('pointerup', up); owner.removeEventListener('pointercancel', cancelDrag); owner.removeEventListener('blur', cancelDrag);
      ghost?.remove(); grid.classList.remove('mc-card-snap-active'); card.classList.remove('mc-card-drag-source'); delete card.dataset.dropLabel; stopGesture = undefined;
      if (active) {
        if (cancel) state.positions[uid] = original;
        else if (target) moveOverviewCard(state, uid, target, after);
        state.sort = cancel ? originalSort : 'manual'; suppressClickUntil = performance.now() + 250;
      }
      update();
    };
    const up = (next: PointerEvent): void => {
      const bounds = scroll.getBoundingClientRect();
      finish(next.clientX < bounds.left || next.clientX > bounds.right || next.clientY < bounds.top || next.clientY > bounds.bottom);
    };
    const cancelDrag = (): void => finish(true);
    stopGesture = cancelDrag;
    owner.addEventListener('pointermove', move, { passive: false }); owner.addEventListener('pointerup', up); owner.addEventListener('pointercancel', cancelDrag); owner.addEventListener('blur', cancelDrag);
  }
  function keyDown(event: KeyboardEvent): void {
    const input = event.target as HTMLInputElement;
    if (input.matches('[data-card-name-edit]') && ['Enter', 'Escape'].includes(event.key) && !event.isComposing) {
      event.preventDefault(); event.stopPropagation(); finishRename(event.key === 'Enter'); return;
    }
    if (event.key === 'Escape' && stopGesture) { event.preventDefault(); event.stopPropagation(); stopGesture(); return; }
    if (event.key === 'Escape' && volume) { volume = undefined; event.preventDefault(); event.stopPropagation(); update(); return; }
    const card = (event.target as HTMLElement).closest<HTMLElement>('.mc-overview-card'); if (!card || event.target !== card) return;
    if (event.key === 'Enter') { event.preventDefault(); card.querySelector<HTMLButtonElement>('[data-action=detail]')!.click(); return; }
    if (!event.altKey || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
    if (isCurrent(card.dataset.uid!)) return;
    event.preventDefault(); const uid = card.dataset.uid!, left = event.key === 'ArrowLeft', right = event.key === 'ArrowRight', up = event.key === 'ArrowUp';
    if (state.layout === 'free') {
      const p = state.positions[uid], stepX = state.snap ? columnPitch : 1, stepY = state.snap ? rowPitch : 1;
      const delta = { x: left ? -stepX : right ? stepX : 0, y: up ? -stepY : event.key === 'ArrowDown' ? stepY : 0 };
      let point = snapOverviewPosition({ x: p.x + delta.x, y: p.y + delta.y }, state.snap);
      const occupied = [...views.keys()].filter(id => id !== uid).map(id => state.positions[id]);
      while (state.snap && occupied.some(other => Math.abs(other.x - point.x) < cardSize.width && Math.abs(other.y - point.y) < cardSize.height)) {
        const next = snapOverviewPosition({ x: point.x + delta.x, y: point.y + delta.y }, true);
        if (next.x === point.x && next.y === point.y) { point = p; break; }
        point = next;
      }
      state.positions[uid] = point;
    }
    else {
      const rows = shown().filter(r => !isCurrent(r.receiver_uid)), index = rows.findIndex(r => r.receiver_uid === uid);
      const columns = getComputedStyle(root.querySelector('.mc-card-grid')!).gridTemplateColumns.split(' ').length;
      const next = rows[index + (left ? -1 : right ? 1 : up ? -columns : columns)];
      if (next) moveOverviewCard(state, uid, next.receiver_uid, !left && !up);
    }
    state.sort = 'manual'; update(); [...root.querySelectorAll<HTMLElement>('.mc-overview-card')].find(el => el.dataset.uid === uid)?.focus();
  }
  for (const element of [root, controls]) {
    element.addEventListener('click', click, { signal: events.signal });
    element.addEventListener('change', change, { signal: events.signal });
  }
  root.addEventListener('input', input, { signal: events.signal }); root.addEventListener('pointerdown', pointerDown, { signal: events.signal }); root.addEventListener('keydown', keyDown, { signal: events.signal });
  root.addEventListener('focusout', event => { if ((event.target as HTMLElement).matches('[data-card-name-edit]')) finishRename(true); }, { signal: events.signal });
  root.addEventListener('dblclick', event => { if (!(event.target as HTMLElement).closest('button,input,select,.mc-ticker-viewport')) (event.target as HTMLElement).closest('.mc-overview-card')?.querySelector<HTMLButtonElement>('[data-action=detail]')?.click(); }, { signal: events.signal });
  const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(resizeCanvas); observer?.observe(root);
  return {
    attach(body: HTMLElement, head: HTMLElement): void {
      if (root.parentElement !== body) { stopGesture?.(); body.replaceChildren(root); }
      if (controls.parentElement !== head) head.insertBefore(controls, head.querySelector('.mc-window-actions'));
      update();
    },
    detach(): void { stopGesture?.(); root.remove(); controls.remove(); for (const [uid, view] of views) releaseProbe(uid, view); },
    update, updateResults, paint, fit,
    dispose(): void { disposed = true; stopGesture?.(); events.abort(); observer?.disconnect(); cardVisibility?.disconnect(); for (const [uid, view] of views) disposeView(uid, view); renderTemplate(nothing, root); root.remove(); controls.remove(); },
  };
}
