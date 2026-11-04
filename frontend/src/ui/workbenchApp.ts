import { clone, receiverHasAudio, receiverHasResults, type ReceiverState, type ReceiverConfig, type ReceiverMode, type ViewBinding, type SceneCommand, type TrackKind, type RecordingTask, type AudioRecordingFormat, type Fault } from '../core/multichannel';
import { getLang, onLangChange, t } from '../core/i18n';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import { WorkbenchSession } from './workbenchSession';
import { mountReceiverDecodeView, type ReceiverDecodeView } from './receiverDecodeView';
import { workbenchTip } from './workbenchTips';
import { patchWorkbench } from './workbenchDom';
import { mountWorkbenchCreate, occupiedBandwidth } from './workbenchCreate';
import { workbenchIcon as icon } from './workbenchIcons';
import { convertUnitValue } from '../core/units';
import { frequencyInput, formatFrequencyValue, syncFrequencyInputs, wireFrequencyInputs } from './frequencyInput';
import { mountRecordingView } from './recordingView';
import { mountRecordingLibrary } from './recordingLibrary';
import { audioDownload, audioFormatMenu, formatAvailable, formatLabel, rememberAudioFormat, savedAudioFormat, syncAudioFormatMenus, wireAudioFormatMenus } from './recordingFormats';
import { mountQuickListen } from './quickListen';
import { mountReceiverPresets } from './receiverPresets';
import { resumeNativeAudio, sdrAudioNeedsGesture } from '../audio/sdrAudio';
import type { DecoderLogEntry } from '../sdr/decoderLog';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const esc = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const button = (action: string, text: string, extra = '', tip = workbenchTip(action)): string => `<button type="button" data-action="${action}" title="${esc(tip)}" ${extra}>${icon(({ create: 'plus', listen: 'volume', results: 'signal', detail: 'external', 'follow-results': 'external', 'channel-view': 'signal', recordings: 'record', 'recording-files': 'folder', 'master-mute': 'muted', 'tab-receive': 'signal', 'tab-recording': 'record', 'tab-output': 'external' } as Record<string, string>)[action] ?? (action.startsWith('rec-') ? 'record' : ''))}<span>${esc(text)}</span></button>`;
const panLabel = (value: number): string => value === 0 ? l('Centre', '居中') : `${value < 0 ? l('L', '左') : l('R', '右')} ${Math.round(Math.abs(value) * 100)}%`;
const labels = () => ({ listen: l('Audio demodulation', '音频解调'), digital: l('Protocol decoding', '协议解码'), iq: l('No demodulation', '不解调') });
const audioSettings = (c: ReceiverConfig, audio: boolean): string => {
  const p = c.processors[0], squelch = p?.squelch_dbm ?? -160, algorithm = p?.nr_algorithm ?? 'wiener';
  return `<fieldset class="mc-channel-audio" data-shared-audio ${audio ? '' : 'hidden'}><legend>${l('Channel audio', '信道音频')}</legend>
    <label class="mc-audio-slider" title="${l('Channel output gain, also used for audio recordings', '此信道的输出增益，也用于音频录制')}"><span>${l('Audio gain', '音频增益')}</span><input name="pcm-gain" type="range" min="0" max="2" step="0.01" value="${p?.pcm_gain ?? 0.5}"><output data-pcm-gain-value>${(p?.pcm_gain ?? 0.5).toFixed(2)}×</output></label>
    <div data-monitor-enhancements ${['drm', 'drmplus'].includes(p?.mode) ? 'hidden' : ''}>
      <label class="mc-audio-slider" title="${esc(t('squelch_tip'))}"><span>${l('Squelch', '静噪')}</span><input name="squelch" type="range" min="-160" max="0" step="0.1" value="${Math.max(-160, squelch)}"><output data-squelch-value>${squelch <= -160 ? t('squelch_off') : `${squelch.toFixed(1)} dBm`}</output></label>
      <div class="mc-audio-options"><span>${l('Processing', '音频处理')}</span><div class="mc-audio-switches">
        <label class="mc-audio-toggle" title="${esc(t('agc_tip'))}"><input name="agc" type="checkbox" ${p?.agc !== false ? 'checked' : ''}><span>AGC</span></label>
        <label class="mc-audio-toggle" title="${l('Enable noise reduction for this channel', '开启此信道的降噪')}"><input name="nr" type="checkbox" ${p?.nr ? 'checked' : ''}><span>NR</span></label>
        <select name="nr-algorithm" aria-label="${l('NR algorithm', 'NR 算法')}" title="${esc(t('nr_algo_tip'))}">${(['wiener', 'dfn', 'gtcrn', 'ul-unas', 'fastenhancer'] as const).map(value => {
        const key = value.replace('-', '_');
        return `<option value="${value}" title="${esc(t(`nr_algo_${key}_tip`))}" ${algorithm === value ? 'selected' : ''}>${esc(t(`nr_algo_${key}`))}</option>`;
      }).join('')}</select></div></div>
      <label class="mc-audio-slider" data-nr-strength ${algorithm === 'wiener' ? '' : 'hidden'} title="${esc(t('nr_strength_tip'))}"><span>${l('NR strength', '降噪强度')}</span><input name="nr-strength" type="range" min="0" max="1" step="0.01" value="${p?.nr_strength ?? 0.6}"><output data-nr-strength-value>${Math.round((p?.nr_strength ?? 0.6) * 100)}%</output></label>
      <label class="mc-audio-slider" data-nr-atten ${algorithm !== 'wiener' ? '' : 'hidden'} title="${esc(t('nr_atten_tip'))}"><span>${l('NR attenuation', '降噪衰减')}</span><input name="nr-atten" type="range" min="0" max="100" step="1" value="${p?.nr_atten_db ?? 10}"><output data-nr-atten-value>${p?.nr_atten_db ?? 10} dB</output></label>
    </div></fieldset>`;
};
const syncAudioSettings = (form: HTMLFormElement): void => {
  const algorithm = form.querySelector<HTMLSelectElement>('[name=nr-algorithm]')?.value ?? 'wiener';
  for (const [selector, hidden] of [['[data-nr-strength]', algorithm !== 'wiener'], ['[data-nr-atten]', algorithm === 'wiener']] as const) {
    const field = form.querySelector<HTMLElement>(selector); if (field) field.hidden = hidden;
  }
  const squelch = Number(form.querySelector<HTMLInputElement>('[name=squelch]')?.value ?? -160);
  const level = form.querySelector('[data-squelch-value]'); if (level) level.textContent = squelch <= -160 ? t('squelch_off') : `${squelch.toFixed(1)} dBm`;
  const strength = form.querySelector('[data-nr-strength-value]');
  if (strength) strength.textContent = `${Math.round(Number(form.querySelector<HTMLInputElement>('[name=nr-strength]')?.value ?? 0.6) * 100)}%`;
  for (const [name, selector, format] of [
    ['pcm-gain', '[data-pcm-gain-value]', (value: number) => `${value.toFixed(2)}×`],
    ['nr-atten', '[data-nr-atten-value]', (value: number) => `${value} dB`],
  ] as const) {
    const input = form.querySelector<HTMLInputElement>(`[name=${name}]`), output = form.querySelector(selector);
    if (input && output) output.textContent = format(Number(input.value));
  }
};
const configForm = (element: Element): HTMLFormElement | null => {
  const form = (element as HTMLInputElement | HTMLSelectElement).form ?? element.closest('form');
  return form?.hasAttribute('data-config') ? form : null;
};
const parameterInput = (name: string, label: string, value: number, unit: string, min = 1): string =>
  `<label class="mc-parameter-row"><span>${label}</span><span class="mc-parameter-input"><input name="${name}" type="number" min="${min}" step="any" value="${value}" required><span class="mc-parameter-unit">${unit}</span></span></label>`;
const bandwidthPresets = (mode: ReceiverMode | undefined, current: number): string => (mode?.bandwidthPresetsHz ?? (mode ? [mode.defaultBandwidthHz] : [])).map(hz =>
  button('bandwidth-preset', hz >= 1000 ? `${hz / 1000} kHz` : `${hz} Hz`, `data-key="bandwidth-${hz}" data-bandwidth="${hz}" aria-pressed="${Math.abs(hz - current) < 0.5}"`, l('Use this demodulation bandwidth; apply to save', '选择此解调带宽，应用设置后生效'))).join('');
const receiverLabel = (state: string): string => ({ running: l('Receiving', '接收中'), stopped: l('Stopped', '已停用'), interrupted: l('Interrupted', '接收中断'), failed: l('Failed', '接收失败') })[state] ?? state;
const taskLabel = (state: string): string => ({ preparing: l('Preparing', '准备'), waiting: l('Waiting', '等待'), recording: l('Recording', '记录中'),
  finalizing: l('Finalizing', '收尾'), completed: l('Completed', '完成'), failed: l('Failed', '失败') })[state] ?? state;
interface Draft { values: Record<string, string | boolean>; revision: string }
interface Pane { window: FloatingWindow; title(): string; kind: 'overview' | 'detail' | 'results'; binding: ViewBinding; tab: string; renderedUid?: string; renderedTab?: string; decode?: ReceiverDecodeView; inlineDecode?: { root: HTMLElement; view: ReceiverDecodeView }; create?: ReturnType<typeof mountWorkbenchCreate>; presets?: { root: HTMLElement; dispose(): void }; dirty?: boolean; expanded: Set<string>; interacting?: boolean; drafts: Map<string, Draft>; service?: string; savedUid?: string; unwireFrequency?(): void; unwireFormats?(): void }

interface NativeViews {
  results(uid: string): { entries: readonly DecoderLogEntry[]; evicted: number };
  status(uid: string): string; retry(uid: string): void;
  playback?(uid: string): Record<string, string | number | boolean> | undefined;
}
export function mountWorkbench(session: WorkbenchSession, host: HTMLElement, native?: NativeViews): { dispose(): void; recall(reset?: boolean): void; showOverview(): void } {
  const fixture = session.snapshot.transport === 'fixture';
  const captureSummary = (): string => {
    const c = session.snapshot.capture;
    const state = c.state === 'running' ? l('Running', '采集中') : c.state === 'idle' ? l('Idle', '未启动') : l('Failed', '采集失败');
    if (c.usable_bandwidth_hz === null || c.input_rate_hz === null || c.parameters_state === 'unknown')
      return `${state} · ${l('Capture parameters unknown', '采集参数未知')}`;
    const label = c.parameters_state === 'last_known' || c.state !== 'running' ? l('Last capture', '上次采集')
      : c.parameters_state === 'simulated' ? l('Simulated window', '模拟窗口') : l('Shared capture', '共享采集');
    return `${state} · ${label}: ${c.center_hz / 1e6} MHz / ${c.usable_bandwidth_hz / 1000} kHz · ${c.input_rate_hz} S/s · epoch ${c.epoch}`;
  };
  const faultText = (fault: Fault, config?: ReceiverConfig): string => {
    if (fault.code !== 'RF_RANGE' && !/^full passband and transition exceed (?:the )?shared capture$/i.test(fault.message)) return fault.message;
    const c = session.snapshot.capture;
    const range = (center: number, half: number): string => `${Number(((center - half) / 1e6).toFixed(6))}–${Number(((center + half) / 1e6).toFixed(6))} MHz`;
    const current = c.state === 'running' && c.parameters_state !== 'unknown' && c.usable_bandwidth_hz !== null
      ? ` ${l('Shared capture', '当前共享采集')}: ${range(c.center_hz, c.usable_bandwidth_hz / 2)}.` : '';
    const needed = config ? ` ${l('Channel requires', '该信道需要')}: ${range(config.baseband.frequency_hz, config.baseband.passband_hz / 2 + config.baseband.transition_hz)}.` : '';
    return l('The full channel passband and transition are outside the shared capture.', '信道完整通带及过渡带超出共享采集范围。') + current + needed
      + l(' Adjust the shared centre or capture bandwidth in the main SDR panel, then retry. Shared capture changes affect all channels.', ' 请在主 SDR 面板调整共享中心或采集带宽，覆盖所需频段后重试；共享采集变更会影响所有信道。');
  };
  const archives = mountRecordingView({ demodulateOffset: () => !!session.snapshot.capabilities.iq_file_demodulate_offset, demodulateModes: () => session.snapshot.capabilities.iq_file_demodulate ? session.snapshot.capabilities.iq_file_demodulate_modes ?? [] : [], collection: () => !!session.snapshot.capabilities.iq_archive_collection, streaming: () => !!session.snapshot.capabilities.iq_archive_export, exportLimit: () => session.snapshot.capabilities.iq_export_max_samples ?? '1048576' });
  let audioArchives: ReturnType<typeof import('./recordingAudioView').mountRecordingAudioView> | null = null;
  let disposed = false;
  let preferredFormat = savedAudioFormat();
  const taskFormats = new Map<string, AudioRecordingFormat>();
  const selectedFormat = (task?: RecordingTask): AudioRecordingFormat => {
    const format = task ? taskFormats.get(task.task_id) ?? task.audio_format ?? 'wav' : preferredFormat;
    return formatAvailable(session.snapshot, format) ? format : 'wav';
  };
  const openAudio = (task: RecordingTask): void => {
    void import('./recordingAudioView').then(module => {
      if (!disposed) { audioArchives ??= module.mountRecordingAudioView(); audioArchives.open(task); }
    });
  };
  const archiveButton = (task: RecordingTask): string => {
    if (fixture || !['completed', 'failed'].includes(task.state)) return '';
    const caps = session.snapshot.capabilities;
    if (caps.audio_archive_export && task.tracks.some(t => t.kind === 'audio')) {
      const play = caps.audio_archive_read && task.tracks.some(t => t.kind === 'audio' && t.segments.some(s => BigInt(s.samples) > 0n))
        ? button('audio-archive', l('Play audio', '音频试听'), `data-archive-task="${esc(task.task_id)}"`, l('Listen to saved audio; live monitoring continues independently', '试听已保存音频；实时监听独立运行')) : '';
      return play + audioDownload(task, selectedFormat(task), session.snapshot);
    }
    if (caps.results_archive_export && task.tracks.some(t => t.kind === 'results')) return `<a class="mc-results-download" href="/api/sdr/recordings/${encodeURIComponent(task.task_id)}/results" download title="${l('Download messages, images and task metadata', '下载消息、图像及录制信息')}">${l('Download results', '下载结果')}</a>`;
    if (caps.iq_archive_read && task.tracks.some(t => t.kind === 'iq' && t.segments.some(s => BigInt(s.samples) > 0n))) return button('archive', l('Waveform / SigMF', '波形回放 / SigMF'), `data-archive-task="${esc(task.task_id)}"`, l('Inspect saved IQ or export files', '回放已保存 IQ 或导出文件'));
    return '';
  };
  const library = mountRecordingLibrary(session, { archive: archiveButton, openAudio, openIq: task => archives.open(task),
    chooseFormat: (id, format) => { if (formatAvailable(session.snapshot, format)) { taskFormats.set(id, format); session.notify(); } } });
  const available = (command: string): boolean => fixture || !!session.snapshot.capabilities.commands?.includes(command);
  const applyCapabilities = (body: HTMLElement): void => {
    if (fixture) return;
    body.querySelectorAll<HTMLButtonElement>('[data-action]').forEach(button => {
      const action = button.dataset.action!;
      if ((action.startsWith('rec-') && !available('record_start')) || ['scene', 'preset-save', 'preset-load'].includes(action) || (['listen', 'solo'].includes(action) && !session.snapshot.capabilities.monitor_mixing)) {
        button.disabled = true; button.title = l('This service does not support this action', '当前服务不支持此操作');
      }
    });
    if (!session.snapshot.capabilities.monitor_mixing) body.querySelectorAll<HTMLElement>('.mc-monitor,footer.mc-toolbar').forEach(element => { element.hidden = true; });
  };
  const panes = new Map<string, Pane>();
  const saveViews = (): void => { try { localStorage.setItem('websa-workbench-views-v1', JSON.stringify([...panes.values()].map(p => ({ kind: p.kind, binding: p.binding, tab: p.tab })))); } catch { /* session only */ } };
  let search = '', channelFilter = 'all', scene: SceneCommand | null = null;
  let preset: ReceiverConfig | null = null;
  const receiver = (uid: string) => session.snapshot.receivers.find(r => r.receiver_uid === uid);
  const send = (command: Parameters<WorkbenchSession['send']>[0]): void => { void session.send(command); };
  const create = (config: ReceiverConfig): void => {
    const previous = new Set(session.snapshot.receivers.map(r => r.receiver_uid));
    void session.send({ type: 'create', request_id: session.requestId(), config }).then(reply => {
      if (reply.fault) return;
      const added = reply.snapshot.receivers.find(r => !previous.has(r.receiver_uid));
      if (added) session.select(added.receiver_uid);
    });
  };
  const recordKind = (kind: TrackKind): boolean => fixture || !!session.snapshot.capabilities.recording_kinds?.includes(kind);
  const activeTask = (uid: string, kind: TrackKind) => session.snapshot.recordings.find(t => t.receiver_uid === uid && t.tracks.some(track => track.kind === kind) && ['preparing', 'recording', 'waiting', 'finalizing'].includes(t.state));
  const size = (bytes: string): string => { const n = BigInt(bytes); return n < 1048576n ? `${n / 1024n} KiB` : `${Number(n / 1024n) / 1024 < 10 ? (Number(n / 1024n) / 1024).toFixed(1) : n / 1048576n} MiB`; };
  const recordButton = (uid: string, kind: TrackKind): string => {
    const task = activeTask(uid, kind);
    const name = kind === 'results' ? l('results', '结果') : kind === 'audio' ? l('audio', '音频') : kind.toUpperCase();
    const control = button(`rec-${kind}`, task ? task.state === 'finalizing' ? l('Finalizing…', '收尾中…') : l(`Stop ${name} REC`, `停止 ${name} 记录`) : l(`Record ${name}`, `录制 ${name}`), `aria-pressed="${!!task}" class="${task ? 'mc-recording-on' : ''}" ${task?.state === 'finalizing' || !task && receiver(uid)?.state !== 'running' ? 'disabled' : ''}`, task ? l('Stop this recording and finish saving the file; reception continues', '停止这项录制并完成文件保存；该路继续接收') : kind === 'audio' ? l('Record published audio with shared gain and processing; local speaker controls are excluded', '录制含共享增益与处理的实际输出音频；不含本地扬声器设置') : l(`Save ${name} on the server; listening and decoding can run at the same time`, `在服务器录制 ${name}；可同时监听和解码`));
    if (kind !== 'audio') return control;
    return `<span class="mc-record-controls">${task ? `<span class="mc-record-format" title="${l('Format selected when recording started', '开始录制时选择的格式')}">${formatLabel(task.audio_format ?? 'wav')}</span>` : audioFormatMenu(session.snapshot, selectedFormat(), { receiver: uid })}${control}</span>`;
  };
  const taskSummary = (r: ReceiverState): string => session.snapshot.recordings.filter(t => t.receiver_uid === r.receiver_uid)
    .slice(-3).map(t => `${t.tracks.map(k => k.kind.toUpperCase()).join('+')}: ${taskLabel(t.state)} · ${size(t.tracks[0]?.stored_bytes ?? '0')}${t.fault ? ` · ${t.fault.message}` : ''}`).join(' / ') || '—';
  const managementButtons = (uid: string): string => {
    const full = session.snapshot.receivers.length >= session.snapshot.capabilities.max_channels;
    const deleting = session.retiring(uid), busy = deleting || !!session.edits.get(uid)?.pending;
    const owner = `data-receiver-uid="${esc(uid)}"`;
    return button('duplicate', l('Duplicate channel', '复制信道'), `${owner} ${full || busy ? 'disabled' : ''}`,
      full ? l('Channel limit reached; delete an unused channel to add one', '信道路数已满，可删除不需要的信道后再添加') : workbenchTip('duplicate'))
      + button('retire', deleting ? l('Deleting…', '正在删除…') : l('Delete channel', '删除信道'), `${owner} ${busy ? 'disabled' : ''}`);
  };
  const duplicateMonitorLabel = (uid: string): string => {
    const owner = receiver(session.duplicateMonitor(uid) ?? '');
    return owner ? l(`Same audio · playing through C${owner.display_number}`, `相同音频 · 由 C${owner.display_number} 播放`) : '';
  };
  const rowHTML = (row: ReceiverState): string => {
    const selected = row.receiver_uid === session.selected, uid = row.receiver_uid, audio = receiverHasAudio(session.snapshot, row.confirmed);
    const recording = session.snapshot.recordings.filter(t => t.receiver_uid === uid && ['preparing', 'recording', 'waiting', 'finalizing'].includes(t.state)).length;
    const recordingFailed = session.snapshot.recordings.filter(t => t.receiver_uid === uid).at(-1)?.state === 'failed';
    return `<article class="mc-row ${selected ? 'mc-selected' : ''}" data-uid="${esc(uid)}" style="--channel:${esc(row.confirmed.color)}">
      <button type="button" data-action="select" class="mc-channel-select" aria-pressed="${selected}" title="${esc(workbenchTip('select'))}">
        <span class="mc-channel-avatar">${row.display_number.toString().padStart(2, '0')}</span><span class="mc-channel-identity"><strong>${esc(row.confirmed.name)}</strong><span>${esc(row.confirmed.processors[0]?.mode.toUpperCase() ?? 'IQ')}<i class="mc-state-dot" data-state="${row.state}"></i>${receiverLabel(row.state)}</span></span>${selected ? '<span class="mc-selection-mark">›</span>' : ''}</button>
      <div class="mc-row-frequency">${(row.confirmed.baseband.frequency_hz / 1e6).toFixed(6)} <small>MHz</small></div>
      <div class="mc-row-actions">${audio ? button('listen', session.listening(uid) ? l('Listening', '监听中') : l('Listen', '监听'), `aria-pressed="${session.listening(uid)}" ${row.state === 'running' && !row.fault ? '' : 'disabled'}`) : row.confirmed.processors.length ? button('results', resultLabel(row.confirmed)) : button('channel-view', l('Open channel', '查看信道'), '', workbenchTip('select'))}
        ${available('record_start') ? button('recordings', recording ? l(`Recording · ${recording}`, `录制中 · ${recording}`) : recordingFailed ? l('Recording failed', '录制失败') : l('Record', '录制'), `class="${recording || recordingFailed ? 'mc-recording-on' : ''}"`) : ''}</div>
      <details class="mc-row-more" data-section="more-${esc(uid)}"><summary>${l('More actions', '更多操作')}${paneDraftCount(uid) ? `<span class="mc-draft-dot">${l('Draft', '草稿')}</span>` : ''}</summary><div class="mc-row-secondary">
        ${audio ? button('solo', l('Solo', '独听'), `aria-pressed="${session.route(uid).solo}" ${session.listening(uid) ? '' : 'disabled'}`) + button('results', resultLabel(row.confirmed)) : ''}
        ${available('record_start') ? `${recordKind('iq') ? recordButton(uid, 'iq') : ''}${recordKind('audio') && (audio || activeTask(uid, 'audio')) ? recordButton(uid, 'audio') : ''}${recordKind('results') && (receiverHasResults(session.snapshot, row.confirmed) || activeTask(uid, 'results')) ? recordButton(uid, 'results') : ''}` : ''}
        ${button('detail', l('Separate window', '独立窗口'))}${managementButtons(uid)}</div></details>
      ${duplicateMonitorLabel(uid) ? `<small class="mc-row-task">${esc(duplicateMonitorLabel(uid))}</small>` : ''}
      ${recording || recordingFailed ? `<small class="mc-row-task">${esc(taskSummary(row))}</small>` : ''}</article>`;
  };
  const paneDraftCount = (uid: string): boolean => [...panes.values()].some(p => p.drafts.has(uid));
  const record = (uid: string, kind: TrackKind): void => {
    const task = activeTask(uid, kind);
    if (task?.state === 'finalizing') return;
    send(task ? { type: 'record_stop', request_id: session.requestId(), task_id: task.task_id }
      : { type: 'record_start', request_id: session.requestId(), receiver_uid: uid, kinds: [kind], ...(kind === 'audio' ? { audio_format: selectedFormat() } : {}) });
  };
  const resultLabel = (c: ReceiverConfig): string => ['am', 'dsb', 'usb', 'lsb', 'nfm', 'wfm', 'pm', 'drm', 'drmplus'].includes(c.processors[0]?.mode) ? l('Signal info', '信号信息') : l('Results', '查看结果');
  const modeOptions = (c: ReceiverConfig): string => `<option value="" ${!c.processors.length ? 'selected' : ''}>${l('None · IQ only', '不解调 · 仅接收 IQ')}</option>` + (session.snapshot.capabilities.mode_descriptors ?? []).map(mode =>
    `<option value="${esc(mode.id)}" ${c.processors[0]?.mode === mode.id ? 'selected' : ''} ${mode.available ? '' : 'disabled'} title="${esc(mode.unavailableReason ?? '')}">${esc(mode.label)}${mode.available ? '' : ` · ${esc(mode.unavailableReason)}`}</option>`).join('');
  const syncBandwidthSettings = (form: HTMLFormElement): void => {
    const field = form.querySelector<HTMLInputElement>('[name=processor-bandwidth]'); if (!field) return;
    const mode = session.snapshot.capabilities.mode_descriptors?.find(m => m.id === form.querySelector<HTMLSelectElement>('[name=mode]')?.value);
    field.min = String(mode?.minBandwidthHz ?? 50);
    if (mode?.maxBandwidthHz !== undefined) field.max = String(mode.maxBandwidthHz); else field.removeAttribute('max');
    field.readOnly = mode?.bandwidthEditable === false;
    const presets = form.querySelector<HTMLElement>('[data-bandwidth-presets]');
    if (presets) patchWorkbench(presets, bandwidthPresets(mode, Number(field.value)));
  };
  const rememberDraft = (pane: Pane): void => {
    const form = pane.window.body.querySelector<HTMLFormElement>('[data-config]');
    if (!pane.dirty || pane.renderedTab !== 'receive' || !pane.renderedUid || !form) return;
    const values: Draft['values'] = {};
    [...form.elements].filter(element => element.matches('input[name],select[name]')).forEach(element => {
      const input = element as HTMLInputElement | HTMLSelectElement;
      values[input.name] = input.type === 'checkbox' ? (input as HTMLInputElement).checked : input.value;
    });
    pane.drafts.set(pane.renderedUid, { values, revision: form.dataset.revision! });
  };
  const restoreDraft = (pane: Pane, uid: string): void => {
    const draft = pane.drafts.get(uid), form = pane.window.body.querySelector<HTMLFormElement>('[data-config]');
    if (!draft || !form || pane.tab !== 'receive') return;
    for (const [name, value] of Object.entries(draft.values)) {
      const input = form.elements.namedItem(name) as HTMLInputElement | HTMLSelectElement | null;
      if (!input) continue;
      if (typeof value === 'boolean') (input as HTMLInputElement).checked = value; else input.value = value;
    }
    form.dataset.revision = draft.revision;
    syncFrequencyInputs(form);
    syncAudioSettings(form);
    syncBandwidthSettings(form);
    const mode = session.snapshot.capabilities.mode_descriptors?.find(m => m.id === draft.values.mode);
    for (const [selector, hidden] of [['[data-process-settings]', !mode], ['[data-shared-audio]', !mode || !(mode.outputs.includes('audio') || mode.audioCompanion !== null)], ['[data-monitor-enhancements]', !mode || ['drm', 'drmplus'].includes(mode.id)]] as const) {
      const element = form.querySelector<HTMLElement>(selector); if (element) element.hidden = hidden;
    }
    pane.dirty = true;
  };
  const draftStatus = (pane: Pane, uid: string): void => {
    const form = pane.window.body.querySelector<HTMLFormElement>('[data-config]'); if (!form) return;
    const pending = session.edits.get(uid)?.pending || session.retiring(uid), stale = form.dataset.revision !== receiver(uid)?.revision;
    const recording = session.snapshot.recordings.some(t => t.receiver_uid === uid && ['preparing', 'recording', 'waiting'].includes(t.state));
    const status = form.querySelector<HTMLElement>('[data-draft-status]')!;
    status.textContent = pending ? l('Applying…', '正在应用…') : pane.dirty ? stale ? l('Channel updated elsewhere · review before applying', '此信道已在其他位置更新，请检查后应用') : recording ? l('Unapplied · signal changes create recording segments', '尚未应用 · 调整信号将为录制新建片段') : l('Unapplied changes', '有未应用的修改') : pane.savedUid === uid ? l('Settings applied', '设置已应用') : '';
    status.classList.toggle('mc-has-draft', !!pane.dirty);
    form.querySelector<HTMLButtonElement>('[type=submit]')!.disabled = !!pending || !pane.dirty;
    form.querySelector<HTMLButtonElement>('[data-action=cancel]')!.disabled = !pane.dirty || !!pending;
  };
  const updateDecode = (view: ReceiverDecodeView, uid: string, r?: ReceiverState): void => {
    const log = native?.results(uid);
    view.update(fixture && r?.confirmed.purpose === 'digital' ? [{ id: 1, at: 1_793_404_800_000, source: r.receiver_uid,
      provider: 'fixture', mode: 'ft8', kind: 'table', bytes: 128,
      data: { decodes: [{ snr_db: -12, audio_hz: 1099, msg: 'FIXTURE CQ TEST AA00' }] } }] : log?.entries ?? [],
      `${session.snapshot.service_id ?? 'fixture'}:${uid}`, r?.confirmed.processors[0]?.mode ?? '', log?.evicted ?? 0,
      r ? r.state === 'running' ? '' : l('Receiver stopped · retained results', '接收已停止 · 保留历史结果') : l('Receiver retired. Select another channel above.', '信道已删除，可在上方选择其他信道。'));
  };
  const detail = (r: ReceiverState, pane: Pane): string => {
    const c = r.confirmed, edit = session.edits.get(r.receiver_uid), route = session.route(r.receiver_uid);
    const formId = `mc-config-${pane.window.root.dataset.viewId}`;
    const mode = session.snapshot.capabilities.mode_descriptors?.find(m => m.id === c.processors[0]?.mode);
    const playback = native?.playback?.(r.receiver_uid);
    const monitorEnabled = session.listening(r.receiver_uid) && r.state === 'running' && !r.fault;
    const tabs = ['receive', 'recording', 'output'];
    const tabNames = [l('Receive & analyse', '接收与分析'), l('Recordings & files', '录制与文件'), l('Live outputs', '实时输出')];
    let content = '';
    const expanded = (key: string): string => pane.expanded.has(key) ? 'open' : '';
    if (!fixture && pane.tab === 'recording' && !available('record_start')) pane.tab = 'receive';
    if (pane.tab === 'receive') content = `<form id="${esc(formId)}" data-key="config-${r.receiver_uid}-receive" data-config data-revision="${r.revision}">
      <div class="mc-tuning-grid"><label class="mc-frequency-field"><span>${l('Receive frequency', '接收频率')}</span>${frequencyInput(c.baseband.frequency_hz)}</label>
      <label class="mc-mode-field"><span>${l('Demodulate / decode', '解调 / 解码')}</span>${fixture ? `<select name="purpose">${Object.entries(labels()).map(([v, label]) => `<option value="${v}" ${c.purpose === v ? 'selected' : ''}>${label}</option>`).join('')}</select>` : `<select name="mode" title="${l('Choose a mode; bandwidth and rate are adjusted automatically', '选择模式后自动匹配带宽与速率')}">${modeOptions(c)}</select>`}</label></div>
      ${!fixture ? audioSettings(c, receiverHasAudio(session.snapshot, c)) : ''}
      <div class="mc-form-actions"><span data-draft-status role="status" class="mc-draft-status"></span>${button('advanced-settings', l('Advanced', '高级设置'), `class="mc-quiet" aria-expanded="${pane.expanded.has('advanced')}"`, l('Show demodulation and IQ bandwidth settings', '展开解调及 IQ 带宽设置'))}${button('cancel', l('Reset', '还原'), 'class="mc-quiet"')}<button type="submit" class="mc-primary" title="${l('Apply these settings to this channel', '将设置应用到此信道')}">${icon('check')}${l('Apply settings', '应用设置')}</button></div>
      <div data-key="preview-${r.receiver_uid}-${pane.tab}" data-preview></div>
      <section class="mc-advanced" data-advanced-settings aria-label="${l('Advanced settings', '高级设置')}" ${pane.expanded.has('advanced') ? '' : 'hidden'}><div class="mc-fields">
        ${!fixture ? `<details data-process-settings ${c.processors.length ? '' : 'hidden'} data-section="processor" ${expanded('processor')}><summary>${l('Demodulation bandwidth', '解调带宽')}</summary><div class="mc-fields mc-processor-fields">
          ${parameterInput('processor-bandwidth', l('Bandwidth', '带宽'), c.processors[0]?.bandwidth_hz ?? 6000, 'Hz', mode?.minBandwidthHz ?? 50)}
          <div class="mc-bandwidth-presets" data-bandwidth-presets role="group" aria-label="${l('Bandwidth presets', '带宽快选')}">${bandwidthPresets(mode, c.processors[0]?.bandwidth_hz ?? 6000)}</div></div></details>` : ''}
        <details data-section="iq" ${expanded('iq')}><summary>${l('IQ bandwidth and rate', 'IQ 带宽与速率')}</summary><div class="mc-fields mc-iq-fields">
          ${parameterInput('bandwidth', l('Full passband', '完整通带'), c.baseband.passband_hz, 'Hz')}
          ${parameterInput('transition', l('Transition', '过渡带'), c.baseband.transition_hz, 'Hz')}
          ${parameterInput('minimum-rate', l('Minimum rate', '最低采样率'), c.baseband.minimum_rate_hz, 'S/s')}</div></details>
      </div></section></form>
      ${!fixture && session.snapshot.capabilities.receiver_presets ? `<details data-key="presets-${esc(session.snapshot.service_id)}-${r.receiver_uid}" data-receiver-presets data-section="presets" ${expanded('presets')}></details>` : ''}
      <section class="mc-live-section" data-key="live-${esc(r.receiver_uid)}"><header class="mc-section-heading"><div>${icon('signal')}<h3>${c.processors.length ? l('Live signal & results', '实时信号与结果') : l('IQ reception', 'IQ 接收')}</h3></div>${c.processors.length ? button('results', l('Open window', '独立查看'), 'class="mc-quiet"') : ''}</header>
      ${c.processors.length ? '<div data-inline-decode></div>' : `<div class="mc-iq-summary"><strong>${r.clock ? Number(r.clock.actual_rate_hz.toFixed(3)).toLocaleString() : '—'} <small>S/s</small></strong><p>${l('Receiving raw IQ. Record the signal for later analysis, or connect external software through live outputs.', '正在接收原始 IQ。可录制信号以便后续分析，或通过实时输出连接外部软件。')}</p><div class="mc-toolbar">${available('record_start') && recordKind('iq') ? recordButton(r.receiver_uid, 'iq') : ''}${button('tab-output', l('Connect software', '连接外部软件'))}</div></div>`}</section>`;
    if (pane.tab === 'output') {
      const outputs = session.snapshot.outputs.filter(o => o.receiver_uid === r.receiver_uid);
      const connected = outputs.some(o => o.kind === 'iq' && ['active', 'starting'].includes(o.state));
      const endpoint = (path: string): string => { const url = new URL(path, location.href); url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'; url.searchParams.set('receiver_uid', r.receiver_uid); return url.href; };
      content = `<div class="mc-output-heading"><h3>${l('Connect your tools', '连接你的分析工具')}</h3><p>${l('Copy a WebSocket address to receive this channel in external software.', '复制 WebSocket 地址，在外部软件中接收此信道的数据。')}</p></div><p data-copy-status class="mc-notice" role="status"></p>
        <dl class="mc-output-links"><dt>IQ · CF32</dt><dd><code>${esc(endpoint('/ws/iq'))}</code>${button('copy-output', l('Copy', '复制'), `data-endpoint="${esc(endpoint('/ws/iq'))}"`, l('Copy IQ WebSocket address', '复制 IQ WebSocket 地址'))}</dd>
        ${receiverHasAudio(session.snapshot, c) ? `<dt>${l('Audio', '音频')} · PCM</dt><dd><code>${esc(endpoint('/ws/audio'))}</code>${button('copy-output', l('Copy', '复制'), `data-endpoint="${esc(endpoint('/ws/audio'))}"`, l('Copy audio WebSocket address', '复制音频 WebSocket 地址'))}</dd>` : ''}
        ${c.processors.length ? `<dt>${l('Results', '解码结果')}</dt><dd><code>${esc(endpoint('/ws/decode'))}</code>${button('copy-output', l('Copy', '复制'), `data-endpoint="${esc(endpoint('/ws/decode'))}"`, l('Copy results WebSocket address', '复制结果 WebSocket 地址'))}</dd>` : ''}</dl>
        <details data-section="iq-preview" ${expanded('iq-preview')}><summary>${l('Check IQ delivery in this page', '在本页检查 IQ 数据接收')}</summary>
        ${button('output', connected ? l('Disconnect IQ preview', '断开本页 IQ') : l('Connect IQ preview', '连接本页 IQ'), `aria-pressed="${connected}"`, l('Receive IQ in this page to check sample delivery; this does not save a file', '在本页接收 IQ 以检查样本传输；此操作不保存文件'))}
        <p>${esc(outputs.map(o => `${o.kind.toUpperCase()} · ${o.state} · ${o.samples ?? '0'} samples · ${o.bytes ?? '0'} bytes ${o.fault?.message ?? ''}`).join(' / ') || l('No local IQ connection', '本页未连接 IQ'))}</p></details>`;
    }
    if (pane.tab === 'recording') content = `<div class="mc-output-heading"><h3>${l('Capture what matters', '记录需要的信号')}</h3><p>${l('Choose what to save. Each recording runs independently of local listening.', '选择需要保存的内容，录制可与本地监听同时进行。')}</p></div><div class="mc-record-options">
      ${(['audio', 'results', 'iq'] as const).filter(kind => recordKind(kind) && (kind === 'iq' || activeTask(r.receiver_uid, kind) || (kind === 'audio' ? receiverHasAudio(session.snapshot, c) : receiverHasResults(session.snapshot, c)))).map(kind => `<article class="mc-record-kind"><div><h3>${kind === 'audio' ? l('Audio recording', '音频录制') : kind === 'results' ? l('Decoded results', '解码结果') : l('Raw IQ signal', '原始 IQ 信号')}</h3><p>${kind === 'audio' ? l('Save demodulated audio for playback and download.', '保存解调后的音频，支持回听与下载。') : kind === 'results' ? l('Save decoded messages, events and received images.', '保存解码消息、事件和接收图像。') : l('Keep the original signal for playback and further analysis.', '保留原始信号，用于回放和后续分析。')}</p></div>${recordButton(r.receiver_uid, kind)}</article>`).join('')}</div>
      <div class="mc-record-heading"><h3>${l('Recording history', '录制记录')}</h3>${button('recording-files', l('All files', '全部文件'), 'class="mc-quiet"', l('Manage recordings from every channel', '管理所有信道的录制文件'))}<span>${session.snapshot.recordings.filter(t => t.receiver_uid === r.receiver_uid).length} ${l('tasks', '项任务')}</span></div>
      ${!session.snapshot.recordings.some(t => t.receiver_uid === r.receiver_uid) ? `<div class="mc-workspace-empty mc-record-empty">${icon('record')}<h2>${l('No recordings yet', '还没有录制记录')}</h2><p>${l('Start a recording above. Stop it here when you are ready to play or export the file.', '从上方开始录制，结束后在此试听或导出文件。')}${fixture ? l(' Demo mode simulates recording without writing files.', '演示模式仅模拟录制，不写入文件。') : ''}</p></div>` : ''}
      ${session.snapshot.recordings.filter(t => t.receiver_uid === r.receiver_uid).slice(-10).reverse().map(t => `<article class="mc-task" data-task-id="${esc(t.task_id)}" data-task-state="${t.state}"><b>${t.tracks.map(track => track.kind.toUpperCase()).join(' + ')} · ${taskLabel(t.state)} · ${size(t.tracks[0]?.stored_bytes ?? '0')}</b>
        ${t.fault ? `<p class="mc-error" role="alert">${esc(t.fault.message)}</p>` : ''}${archiveButton(t)}
        <details data-section="task-${esc(t.task_id)}" ${expanded(`task-${t.task_id}`)}><summary>${l('Compression and stream details', '压缩与数据流详情')}</summary>
        <p>${esc(t.task_id)}<br>${esc(t.service_id ?? '')}</p>${t.tracks.map(track => `<p>${track.kind === 'results' ? 'JSONL' : track.kind === 'audio' ? 'Float32 PCM' : 'CF32 LE'} · ${track.compression} · ${track.segments.length} ${l('segments', '片段')}<br>${l('Source bytes', '原始字节')}: ${track.raw_bytes}<br>${l('Saved bytes', '已保存字节')}: ${track.stored_bytes}${fixture ? ` (${l('fixture, no disk I/O', '模拟，无磁盘写入')})` : ''}</p>
        ${track.segments.map(segment => `<p class="mc-clock">${esc(segment.stream.epoch)} / ${esc(segment.stream.channel_id)} · ${segment.actual_rate_hz} S/s · ${segment.events !== undefined ? `${segment.events} ${l('events', '事件')}` : `${segment.samples} samples`}<br>${l('Source position', '源位置')} ${segment.first_input_index} · ${esc(segment.end_reason ?? '…')}</p>`).join('')}`).join('')}</details></article>`).join('')}`;
    return `<header data-key="channel-heading" class="mc-channel-heading" style="--channel:${esc(c.color)}"><div class="mc-channel-title"><span class="mc-channel-avatar">${r.display_number.toString().padStart(2, '0')}</span><div><p class="mc-eyebrow">${l('Channel name', '信道名称')}</p><label class="mc-name-editor"><input data-channel-name name="name" form="${esc(formId)}" value="${esc(c.name)}" maxlength="128" required aria-label="${l('Channel name', '信道名称')}" title="${l('Edit the channel name; press Enter or Apply settings to save', '修改信道名称，按 Enter 或应用设置保存')}" ${session.retiring(r.receiver_uid) ? 'disabled' : ''}>${icon('edit')}</label></div></div><div data-key="channel-actions" class="mc-channel-actions"><button type="button" data-action="toggle-receiver" role="switch" aria-checked="${c.enabled}" title="${c.enabled ? l('Stop receiving this channel', '停止此信道的接收') : l('Start receiving this channel', '开始此信道的接收')}" ${edit?.pending || session.retiring(r.receiver_uid) ? 'disabled' : ''}><span>${receiverLabel(r.state)}</span><span class="mc-receiver-switch" aria-hidden="true"></span></button>${managementButtons(r.receiver_uid)}</div></header>
      ${pane.tab === 'output' ? `<details data-section="clock" ${expanded('clock')}><summary>${l('Stream details', '数据流详情')}</summary><p class="mc-clock">${l('Revision', '修订')} ${r.revision}<br>${esc(r.stream ? `${r.stream.source_id} / epoch ${r.stream.epoch} / channel ${r.stream.channel_id}` : l('No continuous stream', '当前无连续流'))}<br>${r.clock?.actual_rate_hz ?? '—'} S/s</p></details>` : ''}
      ${r.fault ? `<p data-key="receiver-fault" class="mc-error" role="alert">${esc(r.fault.scope)}: ${esc(r.fault.message)}</p>` : ''}
      ${native?.status(r.receiver_uid) ? `<p data-key="consumer-fault" class="mc-error" role="alert">${esc(native.status(r.receiver_uid))} ${button('retry-stream', l('Retry local consumer', '重试本地消费'))}</p>` : ''}
      ${edit?.pending && pane.tab !== 'receive' ? `<p data-key="pending" role="status">${l('Applying settings…', '正在应用设置…')}</p>` : ''}
      ${edit?.fault ? `<p data-key="edit-fault" class="mc-error" role="alert">${esc(edit.fault.code)}: ${esc(faultText(edit.fault, edit.attempted))} ${button('retry', l('Retry', '重试'))}</p>` : ''}
      <nav data-key="tabs" class="mc-tabs" aria-label="${l('Channel workspace', '信道工作区')}">${tabs.map((tab, i) => tab === 'recording' && !available('record_start') ? '' : button(`tab-${tab}`, tabNames[i], `aria-pressed="${pane.tab === tab}"`)).join('')}</nav>
      ${receiverHasAudio(session.snapshot, c) ? `<fieldset data-key="monitor" class="mc-monitor"><legend>${fixture ? l('Monitor · simulated', '监听 · 模拟') : l('Monitor', '监听')}</legend>
        ${button('listen', session.listening(r.receiver_uid) ? l('Listening', '监听中') : l('Listen', '监听'), `aria-pressed="${session.listening(r.receiver_uid)}" ${r.state === 'running' && !r.fault ? '' : 'disabled'}`)} ${button('solo', l('Solo', '独听'), `aria-pressed="${route.solo}" ${session.listening(r.receiver_uid) ? '' : 'disabled'}`)}
        <label title="${l('Speaker volume for this channel in this browser', '此信道在当前浏览器的监听音量')}">${l('Volume', '音量')}<input data-key="gain-${esc(r.receiver_uid)}" aria-label="${l('Local volume', '本地音量')}" data-gain type="range" min="0" max="1" step="0.01" value="${route.gain}" ${monitorEnabled ? '' : 'disabled'}><output data-gain-value>${Math.round(route.gain * 100)}%</output></label>
        <label title="${Number(playback?.outputChannels ?? 2) < 2 ? l('This output device is mono', '当前输出设备为单声道') : l('Left/right position in stereo speakers or headphones', '立体声扬声器或耳机中的左右位置')}">${l('Pan', '声像')}<span class="mc-pan-end">L</span><input data-key="pan-${esc(r.receiver_uid)}" aria-label="${l('Local pan', '本地声像')}" data-pan type="range" min="-1" max="1" step="0.1" value="${route.pan}" ${monitorEnabled && Number(playback?.outputChannels ?? 2) > 1 ? '' : 'disabled'}><span class="mc-pan-end">R</span><output data-pan-value>${Number(playback?.outputChannels ?? 2) < 2 ? l('Mono', '单声道') : panLabel(route.pan)}</output></label>
        <span data-monitor-state>${r.fault ? l('Audio unavailable', '音频不可用') : r.state !== 'running' || !session.listening(r.receiver_uid) ? '' : session.masterMuted || session.masterGain === 0 ? l('Master muted', '总静音') : route.gain === 0 ? l('Muted', '静音') : duplicateMonitorLabel(r.receiver_uid) || (!session.audible(r.receiver_uid) ? l('Solo muted', '独听已屏蔽') : '')}</span>
        ${playback?.enabled && (playback.connectionState === 'reconnecting' || playback.contextState === 'suspended') ? `<small role="status" data-playback-state>${playback.connectionState === 'reconnecting' ? l('Reconnecting…', '重连中…') : l('Enable speakers', '启用扬声器')}${playback.contextState === 'suspended' ? button('resume-audio', l('Enable', '启用')) : ''}</small>` : ''}</fieldset>` : ''}
      ${content}`;
  };
  function renderPane(pane: Pane): void {
    if (pane.interacting) return;
    const uid = session.resolve(pane.binding), r = receiver(uid), body = pane.window.body;
    if (pane.service !== session.snapshot.service_id) { pane.drafts.clear(); pane.dirty = false; pane.renderedUid = undefined; pane.service = session.snapshot.service_id; }
    for (const draftUid of pane.drafts.keys()) if (!receiver(draftUid)) pane.drafts.delete(draftUid);
    // Native details activation precedes its asynchronous toggle event. Read the live state
    // before patching, including a refresh scheduled by releasing a pressed control.
    body.querySelectorAll<HTMLDetailsElement>('details[data-section]').forEach(section => {
      if (section.open) pane.expanded.add(section.dataset.section!); else pane.expanded.delete(section.dataset.section!);
    });
    if (pane.window.root.getAttribute('aria-label') !== pane.title()) pane.window.sync();
    const sameEditor = pane.renderedUid === uid && pane.renderedTab === pane.tab;
    if (!sameEditor) rememberDraft(pane);
    body.classList.toggle('mc-overview-body', pane.kind === 'overview');
    const focused = body.ownerDocument.activeElement;
    const patch = (html: string): void => patchWorkbench(body, html, element =>
      element.hasAttribute('data-decode-view') || element.hasAttribute('data-inline-decode') || element.hasAttribute('data-receiver-presets') || (element.hasAttribute('data-scene') && !!scene) || (sameEditor && (
        (element.hasAttribute('data-config') && (!!pane.dirty || !!focused && element.contains(focused)))
        || (element.hasAttribute('data-channel-name') && !!pane.dirty)
        || (element.hasAttribute('data-preview') && !!pane.dirty)
        || (element === focused && element.matches('input,select,textarea')))));
    pane.renderedUid = uid; pane.renderedTab = pane.tab;
    if (!sameEditor) pane.dirty = pane.drafts.has(uid) && pane.tab === 'receive';
    if (pane.kind === 'overview') {
      const all = session.snapshot.receivers;
      const recording = (uid: string): boolean => session.snapshot.recordings.some(t => t.receiver_uid === uid && ['preparing', 'recording', 'waiting', 'finalizing'].includes(t.state));
      const rows = all.filter(r => `${r.confirmed.name} ${r.confirmed.baseband.frequency_hz} ${r.confirmed.baseband.frequency_hz / 1e6} MHz ${r.confirmed.baseband.frequency_hz / 1000} kHz ${r.confirmed.processors[0]?.mode ?? 'IQ'} ${r.confirmed.tags.join(' ')}`.toLowerCase().includes(search.trim().toLowerCase())
        && (channelFilter === 'all' || channelFilter === 'listening' && receiverHasAudio(session.snapshot, r.confirmed) && session.listening(r.receiver_uid) || channelFilter === 'recording' && recording(r.receiver_uid)));
      const capture = session.snapshot.capture;
      const history = !fixture && session.snapshot.recordings.some(t => !receiver(t.receiver_uid)) ? `<details class="mc-history" data-section="recording-history" ${pane.expanded.has('recording-history') ? 'open' : ''}><summary>${l('Earlier recording tasks', '历史记录任务')}</summary>${session.snapshot.recordings.filter(t => !receiver(t.receiver_uid)).slice(-20).reverse().map(t => `<div class="mc-clock" data-task-id="${esc(t.task_id)}">${esc(t.task_id)} · ${taskLabel(t.state)} · ${size(t.tracks[0]?.stored_bytes ?? '0')}<br>${esc(t.fault?.message ?? '')}${archiveButton(t)}</div>`).join('')}</details>` : '';
      patch(`<header data-key="workspace-head" class="mc-workspace-head"><div><h1>${l('Channel workbench', '信道工作台')}<span class="mc-workspace-count">${all.length} / ${session.snapshot.capabilities.max_channels}</span></h1></div><div class="mc-workspace-actions">${fixture || capture.source_kind === 'fixture' ? `<span class="mc-source-badge">${fixture ? l('Demo', '演示模式') : l('Synthetic source', '模拟源')}</span>` : ''}${button('follow-results', l('Results window', '结果窗口'), `class="mc-quiet" aria-label="${l('Results window', '结果窗口')}"`)}${button('recording-files', l('Recording files', '录制文件'), 'class="mc-quiet"', l('Find, play, download, rename or delete recordings', '查找、试听、下载、重命名或删除录制文件'))}${button('create', l('Add channel', '添加信道'), `class="mc-primary" ${all.length >= session.snapshot.capabilities.max_channels || !available('create') ? 'disabled' : ''}`, all.length >= session.snapshot.capabilities.max_channels ? l('Channel limit reached; delete an unused channel to add one', '信道路数已满，可删除不需要的信道后再添加') : workbenchTip('create'))}</div></header>
      <details data-key="capture-bar" class="mc-capture-bar" data-section="capture" ${pane.expanded.has('capture') ? 'open' : ''}><summary><span><i class="mc-state-dot" data-state="${capture.state}"></i>${l('Shared capture', '共享采集')}<strong>${capture.center_hz / 1e6} MHz</strong><span class="mc-capture-band">${capture.usable_bandwidth_hz === null ? l('Unknown bandwidth', '带宽未知') : `${capture.usable_bandwidth_hz / 1000} kHz`}</span></span><span>${all.filter(r => r.state === 'running').length} ${l('receiving', '路接收中')}</span></summary><p data-capture-status class="mc-clock">${esc(captureSummary())}</p>${fixture ? `<p class="mc-notice">${l('Simulated data. No actual reception, audio or saved files.', '当前为模拟数据，不进行实际接收、播放或文件写入。')}</p>` : ''}</details>
      ${session.fault ? `<p data-key="session-fault" class="mc-error" role="alert">${esc(session.fault.code)}: ${esc(faultText(session.fault))} ${button('dismiss-fault', l('Dismiss', '关闭'))}</p>` : ''}
      <div data-key="content" class="mc-content"><aside data-key="sidebar" class="mc-sidebar" aria-label="${l('Channels', '信道列表')}"><div class="mc-sidebar-head"><h2>${l('Channels', '我的信道')}</h2><span>${all.length}</span></div><label data-key="search" class="mc-search">${icon('search')}<input data-search type="search" aria-label="${l('Find channel by name, frequency or mode', '按名称、频率或模式查找信道')}" placeholder="${l('Name, frequency or mode', '名称、频率或模式')}" value="${esc(search)}"></label>
      <nav class="mc-list-filters" aria-label="${l('Filter channels', '筛选信道')}">${[['all', l('All', '全部')], ['listening', l('Listening', '监听')], ['recording', l('Recording', '录制')]].map(([value, label]) => button(`filter-${value}`, label, `aria-pressed="${channelFilter === value}"`, l(`Show ${value} channels`, `筛选${label}信道`))).join('')}</nav>
      <div data-key="list" class="mc-list">${rows.length ? rows.map(rowHTML).join('') : `<div class="mc-list-empty">${icon('search')}<strong>${all.length ? l('No matching channels', '没有匹配的信道') : l('Your channels appear here', '信道将显示在这里')}</strong><p>${all.length ? l('Try another name, frequency or filter.', '试试其他名称、频率或筛选条件。') : l('Add a frequency to get started.', '添加一个频率，开始接收。')}</p>${all.length ? button('clear-search', l('Clear filters', '清除筛选'), '', l('Clear the search and show all channels', '清除搜索，显示全部信道')) : ''}</div>`}</div>
      <div class="mc-sidebar-footer"><span>${l('Capacity', '信道容量')}</span><strong>${all.length} / ${session.snapshot.capabilities.max_channels}</strong><meter min="0" max="${session.snapshot.capabilities.max_channels}" value="${all.length}" aria-label="${l('Channel capacity', '信道容量')}"></meter>${all.length >= session.snapshot.capabilities.max_channels ? `<small>${l('All slots in use', '路数已满，可在管理信道中释放')}</small>` : ''}</div>
      ${fixture ? `<details class="mc-fixture-tools" data-section="fixture-tools"><summary>${l('Simulation tools', '模拟工具')}</summary><div class="mc-toolbar">${button('preset-save', l('Save preset', '保存预设'))}${button('preset-load', l('Load preset', '载入预设'), preset ? '' : 'disabled')}${button('scene', l('Scene preview', '场景预览'))}</div></details>` : ''}</aside>
      <section data-key="editor" class="mc-editor" data-editor aria-label="${l('Selected channel', '所选信道')}">${r ? detail(r, pane) : `<div class="mc-workspace-empty">${icon('channels')}<p class="mc-eyebrow">READY TO RECEIVE</p><h2>${l('Start with a frequency', '从一个频率开始')}</h2><p>${l('Add a channel to listen, decode signals and save recordings in one workspace.', '添加信道，在同一工作区完成监听、信号解码和录制。')}</p>${button('create', l('Add your first channel', '添加第一个信道'), 'class="mc-primary"')}</div>`}${history}<div data-key="scene" data-scene></div></section></div>
      <footer data-key="master" class="mc-toolbar mc-master"><div class="mc-master-label">${icon('volume')}<span>${l('Speakers', '扬声器')}<small>${all.filter(r => session.listening(r.receiver_uid) && receiverHasAudio(session.snapshot, r.confirmed)).length} ${l('channels selected', '路已加入监听')}</small></span></div>
      ${button('master-mute', session.masterMuted ? l('Unmute', '取消静音') : l('Mute', '静音'), `aria-pressed="${session.masterMuted}"`)}<label>${l('Master volume', '总音量')}<input aria-label="${l('Master volume', '总音量')}" data-master type="range" min="0" max="1" step="0.01" value="${session.masterGain}"><output data-master-value>${Math.round(session.masterGain * 100)}%</output></label>
      <label class="mc-mix-rule"><select data-monitor-mode aria-label="${l('Monitor rule', '监听规则')}" title="${l('Mix channels manually, or listen to the selected channel', '手动混听，或只监听当前选中的信道')}"><option value="manual">${l('Manual mix', '手动混听')}</option><option value="follow">${l('Follow selection', '跟随所选信道')}</option></select></label>${sdrAudioNeedsGesture() ? button('resume-audio', l('Enable speakers', '启用扬声器'), 'class="mc-primary"') : ''}</footer>`);
      body.querySelector<HTMLSelectElement>('[data-monitor-mode]')!.value = session.monitorMode;
      if (scene) showScene(pane);
    } else if (pane.kind === 'detail') patch(r ? detail(r, pane) : l('Receiver retired.', '信道已删除。'));
    else {
      patch(`<div data-key="decode-toolbar" class="mc-decode-binding"><label>${l('Channel', '信道')} <select data-binding aria-label="${l('Decode channel', '解码信道')}"><option value="follow">${l('Follow selection', '跟随选择')}</option>
        ${!r && pane.binding.type === 'fixed' ? `<option value="${esc(uid)}" disabled>${l('Receiver retired', '信道已删除')}</option>` : ''}
        ${session.snapshot.receivers.map(row => `<option value="${row.receiver_uid}">C${row.display_number} · ${esc(row.confirmed.name)}</option>`).join('')}</select></label>
        ${r && receiverHasAudio(session.snapshot, r.confirmed) ? button('listen', l('Listen', '监听'), `aria-pressed="${session.listening(uid)}"`) : ''}
        ${r && available('record_start') && recordKind('iq') ? recordButton(uid, 'iq') : ''}${r && available('record_start') && recordKind('results') && (receiverHasResults(session.snapshot, r.confirmed) || activeTask(uid, 'results')) ? recordButton(uid, 'results') : ''}
        <span class="mc-decode-source">${esc(r ? `${(r.confirmed.baseband.frequency_hz / 1e6).toFixed(6)} MHz · ${r.confirmed.processors[0]?.mode.toUpperCase() ?? 'IQ'}` : l('Receiver retired', '信道已删除'))}</span></div>
        ${native?.status(uid) ? `<p data-key="decode-error" class="mc-error" role="alert">${esc(native.status(uid))}${button('retry-stream', l('Retry', '重试'))}</p>` : ''}
        <div data-key="decode-view" data-decode-view></div>`);
      body.querySelector<HTMLSelectElement>('[data-binding]')!.value = pane.binding.type === 'follow' ? 'follow' : pane.binding.receiver_uid;
      pane.decode ??= mountReceiverDecodeView(body.querySelector<HTMLElement>('[data-decode-view]')!);
      updateDecode(pane.decode, uid, r);
    }
    if (!sameEditor) { restoreDraft(pane, uid); const editor = body.querySelector<HTMLElement>('[data-editor]'); if (editor) editor.scrollTop = 0; }
    const config = body.querySelector<HTMLFormElement>('[data-config]'); if (config) syncBandwidthSettings(config);
    draftStatus(pane, uid);
    const inlineRoot = body.querySelector<HTMLElement>('[data-inline-decode]');
    if (pane.inlineDecode?.root !== inlineRoot) {
      pane.inlineDecode?.view.dispose(); pane.inlineDecode = undefined;
      if (inlineRoot) pane.inlineDecode = { root: inlineRoot, view: mountReceiverDecodeView(inlineRoot) };
    }
    if (pane.inlineDecode) updateDecode(pane.inlineDecode.view, uid, r);
    pane.window.root.dataset.receiverUid = uid;
    const presetsRoot = body.querySelector<HTMLElement>('[data-receiver-presets]');
    if (pane.presets?.root !== presetsRoot) {
      pane.presets?.dispose(); pane.presets = undefined;
      if (presetsRoot) pane.presets = { root: presetsRoot, dispose: mountReceiverPresets(presetsRoot, session, uid, config => preview(pane, uid, config)) };
    }
    applyCapabilities(body);
    syncAudioFormatMenus(body);
  }
  function showScene(pane: Pane): void {
    if (!scene) return;
    const impact = session.adapter.preflight(scene), target = pane.window.body.querySelector('[data-scene]');
    if (target) patchWorkbench(target as HTMLElement, `<div class="mc-preview"><b>${l('Atomic scene preflight', '场景原子预检')}</b><p>${l('Shared center', '共享中心')}: ${session.snapshot.capture.center_hz} → ${scene.capture_center_hz} Hz</p>
      <p>${scene.configs.map(change => `${esc(change.receiver_uid)}: ${receiver(change.receiver_uid)?.confirmed.baseband.frequency_hz} → ${change.config.baseband.frequency_hz} Hz`).join('<br>')}</p><p>${l('Receivers', '信道')}: ${impact.affected_receivers.join(', ')}<br>${l('Recording tracks / new segments', '记录轨道 / 新片段')}: ${esc(impact.affected_tracks.join(', ') || '—')}</p>
      <p>${esc(impact.faults.map(f => f.message).join('; '))} · CF32 ${impact.estimated_cf32_bytes_per_second} B/s (${l('fixture estimate', '模拟估计')})</p>
      ${button('scene-apply', l('Apply scene', '应用场景'), impact.accepted ? '' : 'disabled')}${button('scene-cancel', l('Cancel', '取消'))}</div>`);
  }
  function open(kind: Pane['kind'], binding: ViewBinding = { type: 'follow' }): void {
    const id = `${kind}-${binding.type === 'follow' ? 'follow' : binding.receiver_uid}`;
    const old = panes.get(id); if (old) { renderPane(old); old.window.show(); if (old.window.portal.isPoppedOut()) old.window.portal.popout(); return; }
    const index = panes.size;
    const title = (): string => {
      if (kind === 'overview') return fixture ? l('Channel workbench · FIXTURE', '信道工作台 · FIXTURE') : l('Channel workbench', '信道工作台');
      const target = panes.get(id)?.binding ?? binding, r = receiver(session.resolve(target));
      return `${kind === 'results' ? l('Signal / results', '信号 / 结果') : l('Channel settings', '信道设置')} · ${target.type === 'follow' ? l('Follow', '跟随') : r ? `C${r.display_number} · ${r.confirmed.name}` : l('Receiver retired', '信道已删除')}`;
    };
    const surface = createFloatingWindow(id, title, kind === 'overview' ? { x: 48, y: 70, w: Math.min(1280, window.innerWidth - 64), h: Math.min(860, window.innerHeight - 96) }
      : { x: 170 + index * 35, y: 180 + index * 25, w: kind === 'results' ? 620 : 500, h: kind === 'results' ? 520 : 470 });
    const pane: Pane = { kind, binding, window: surface, title, tab: 'receive', expanded: new Set(), drafts: new Map(), service: session.snapshot.service_id }; panes.set(id, pane);
    pane.unwireFrequency = wireFrequencyInputs(surface.body);
    pane.unwireFormats = wireAudioFormatMenus(surface.body);
    surface.body.addEventListener('toggle', event => {
      const section = event.target as HTMLDetailsElement;
      if (section.matches('details[data-section]') && section.isConnected) {
        if (section.open) pane.expanded.add(section.dataset.section!); else pane.expanded.delete(section.dataset.section!);
      }
    }, true);
    let gestureDocument: Document | null = null;
    const clearGesture = (): void => {
      gestureDocument?.removeEventListener('pointerup', release, true);
      gestureDocument?.removeEventListener('pointercancel', release, true);
      gestureDocument?.defaultView?.removeEventListener('blur', release);
      gestureDocument = null;
    };
    const release = (): void => {
      clearGesture();
      setTimeout(() => { pane.interacting = false; if (surface.root.isConnected) renderPane(pane); }, 0);
    };
    surface.body.addEventListener('pointerdown', () => {
      clearGesture(); pane.interacting = true;
      gestureDocument = surface.body.ownerDocument;
      gestureDocument.addEventListener('pointerup', release, true);
      gestureDocument.addEventListener('pointercancel', release, true);
      gestureDocument.defaultView?.addEventListener('blur', release);
      if (!fixture) resumeNativeAudio();
    }, true);
    // Keep the pressed element until click, including synchronous audio-resume notifications.
    // Bind release to the current document so Pop out/Dock and releasing outside work too.
    surface.body.addEventListener('click', () => { clearGesture(); pane.interacting = false; }, true);
    surface.body.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        const menu = (event.target as HTMLElement).closest<HTMLDetailsElement>('[data-format-menu]');
        if (menu?.open) { menu.open = false; menu.querySelector('summary')?.focus(); event.preventDefault(); event.stopPropagation(); return; }
      }
      if (fixture) return;
      resumeNativeAudio();
      if (event.key.toLowerCase() === 'm' && !event.ctrlKey && !event.metaKey && !event.altKey && !event.isComposing
        && !(event.target as HTMLElement).closest('input,select,textarea,[contenteditable=true]')) {
        event.preventDefault(); event.stopPropagation(); session.masterMuted = !session.masterMuted; session.notify();
      }
    });
    surface.body.addEventListener('click', event => {
      const target = (event.target as HTMLElement).closest<HTMLElement>('[data-action]');
      if (!target) {
        const row = (event.target as HTMLElement).closest<HTMLElement>('[data-uid]');
        if (row && !(event.target as HTMLElement).closest('details,a,input,select')) session.select(row.dataset.uid!);
        return;
      }
      if (target.matches('button:disabled')) return;
      if (target.tagName === 'BUTTON' && (target as HTMLButtonElement).type !== 'submit') event.preventDefault();
      const uid = target.dataset.receiverUid ?? target.closest<HTMLElement>('[data-uid]')?.dataset.uid ?? session.resolve(pane.binding), r = receiver(uid);
      const action = target.dataset.action!;
      if (action.startsWith('tab-')) { pane.tab = action.slice(4); saveViews(); renderPane(pane); }
      else if (action.startsWith('filter-')) { channelFilter = action.slice(7); renderPane(pane); }
      else if (action === 'clear-search') { search = ''; channelFilter = 'all'; const input = surface.body.querySelector<HTMLInputElement>('[data-search]'); if (input) input.value = ''; renderPane(pane); }
      else if (action === 'dismiss-fault') { session.fault = null; session.notify(); }
      else if (action === 'bandwidth-preset') {
        const field = target.closest('form')?.querySelector<HTMLInputElement>('[name=processor-bandwidth]');
        if (field) { field.value = target.dataset.bandwidth!; field.dispatchEvent(new Event('input', { bubbles: true })); }
      }
      else if (action === 'advanced-settings') {
        const section = surface.body.querySelector<HTMLElement>('[data-advanced-settings]');
        if (section) { section.hidden = !section.hidden; if (section.hidden) pane.expanded.delete('advanced'); else pane.expanded.add('advanced'); target.setAttribute('aria-expanded', String(!section.hidden)); }
      }
      else if (action === 'select' || action === 'channel-view') session.select(uid);
      else if (action === 'recordings') { pane.tab = 'recording'; session.select(uid); renderPane(pane); }
      else if (action === 'create') {
        pane.create ??= mountWorkbenchCreate(session, surface.root, added => { pane.tab = 'receive'; search = ''; channelFilter = 'all'; session.select(added); });
        pane.create.open();
      }
      else if (action === 'detail') { session.select(uid); open('detail', { type: 'fixed', receiver_uid: uid }); }
      else if (action === 'results') open('results', { type: 'fixed', receiver_uid: uid });
      else if (action === 'follow-results') open('results');
      else if (action === 'recording-files') library.open();
      else if (action === 'choose-audio-format') {
        const format = target.dataset.format as AudioRecordingFormat;
        if (!formatAvailable(session.snapshot, format)) return;
        const menu = target.closest<HTMLDetailsElement>('[data-format-menu]'); if (menu) menu.open = false;
        if (target.dataset.formatTask) taskFormats.set(target.dataset.formatTask, format);
        else { preferredFormat = format; rememberAudioFormat(format); }
        session.notify(); menu?.querySelector('summary')?.focus();
      }
      else if (action === 'toggle-receiver' && r) void session.configure(uid, { ...clone(r.confirmed), enabled: !r.confirmed.enabled });
      else if (action === 'listen') session.monitor(uid, { enabled: !session.listening(uid) });
      else if (action === 'resume-audio') resumeNativeAudio();
      else if (action === 'master-mute') { session.masterMuted = !session.masterMuted; session.notify(); }
      else if (action === 'retry-stream') native?.retry(uid);
      else if (action === 'solo') session.monitor(uid, { solo: !session.route(uid).solo });
      else if (action.startsWith('rec-')) record(uid, action.slice(4) as TrackKind);
      else if (action === 'audio-archive') {
        const task = session.snapshot.recordings.find(t => t.task_id === target.dataset.archiveTask);
        if (task) openAudio(task);
      }
      else if (action === 'archive') {
        const task = session.snapshot.recordings.find(t => t.task_id === target.dataset.archiveTask);
        if (task) archives.open(task);
      }
      else if (action === 'copy-output') {
        const status = surface.body.querySelector<HTMLElement>('[data-copy-status]');
        const clipboard = surface.body.ownerDocument.defaultView?.navigator.clipboard;
        if (clipboard) void clipboard.writeText(target.dataset.endpoint!).then(() => { if (status?.isConnected) status.textContent = l('Address copied', '地址已复制'); }, () => { if (status?.isConnected) status.textContent = l('Select the address above and copy it.', '请选中上方地址复制。'); });
        else if (status) status.textContent = l('Select the address above and copy it.', '请选中上方地址复制。');
      }
      else if (action === 'output') send({ type: 'output', request_id: session.requestId(), receiver_uid: uid,
        enabled: !session.snapshot.outputs.some(o => o.receiver_uid === uid && ['active', 'starting'].includes(o.state)) });
      else if (action === 'duplicate') {
        if (r) create({ ...clone(r.confirmed), name: `${r.confirmed.name} ${l('copy', '副本')}` });
      } else if (action === 'retire' && r && !session.edits.get(uid)?.pending) send({ type: 'retire', request_id: session.requestId(), receiver_uid: uid });
      else if (action === 'preset-save' && r) { preset = clone(r.confirmed); session.notify(); }
      else if (action === 'preset-load' && preset) preview(pane, uid, clone(preset));
      else if (action === 'cancel') {
        const edit = session.edits.get(uid);
        if (edit && !edit.pending) { if (session.fault === edit.fault) session.fault = null; session.edits.delete(uid); }
        pane.drafts.delete(uid); pane.renderedUid = undefined; pane.dirty = false; session.notify();
      }
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
      event.preventDefault(); const form = event.target as HTMLFormElement, uid = session.resolve(pane.binding), r = receiver(uid); if (!r || session.retiring(uid)) return;
      const data = new FormData(form);
      if (!fixture) {
        const config = clone(r.confirmed), mode = session.snapshot.capabilities.mode_descriptors?.find(m => m.id === data.get('mode'));
        config.name = String(data.get('name')); config.baseband = { frequency_hz: convertUnitValue(Number(data.get('frequency')), String(data.get('frequency-unit') ?? 'Hz'), 'Hz'),
          passband_hz: Number(data.get('bandwidth')), transition_hz: Number(data.get('transition')), minimum_rate_hz: Number(data.get('minimum-rate')) };
        config.purpose = mode ? mode.kind === 'analog' ? 'listen' : 'digital' : 'iq';
        if (config.purpose === 'iq') config.processors = [];
        else if (mode) config.processors = [{ id: 'primary', kind: config.purpose === 'listen' ? 'demodulator' : 'decoder', mode: mode.id,
          minimum_rate_hz: mode.minimumRateHz, bandwidth_hz: Number(data.get('processor-bandwidth') ?? (config.processors[0]?.mode === mode.id ? config.processors[0]?.bandwidth_hz : undefined) ?? mode.defaultBandwidthHz),
          pcm_gain: Number(data.get('pcm-gain') ?? config.processors[0]?.pcm_gain ?? 0.5),
          agc: data.has('agc'),
          squelch_dbm: Number(data.get('squelch') ?? config.processors[0]?.squelch_dbm ?? -160),
          nr: data.has('nr'), nr_algorithm: String(data.get('nr-algorithm') ?? 'wiener') as NonNullable<ReceiverConfig['processors'][number]['nr_algorithm']>,
          nr_strength: Number(data.get('nr-strength') ?? 0.6), nr_atten_db: Number(data.get('nr-atten') ?? 10) }];
        if (mode) {
          const occupied = occupiedBandwidth(mode, config.processors[0].bandwidth_hz!);
          config.baseband.passband_hz = Math.max(config.baseband.passband_hz, occupied);
          config.baseband.minimum_rate_hz = Math.max(config.baseband.minimum_rate_hz, mode.minimumRateHz);
        }
        preview(pane, uid, config, true); return;
      }
      const purpose = String(data.get('purpose')) as ReceiverConfig['purpose'];
      const config = clone(r.confirmed); config.name = String(data.get('name')); config.purpose = purpose;
      config.baseband = { ...config.baseband, frequency_hz: convertUnitValue(Number(data.get('frequency')), String(data.get('frequency-unit') ?? 'Hz'), 'Hz'),
        passband_hz: Number(data.get('bandwidth')), transition_hz: Number(data.get('transition')), minimum_rate_hz: Number(data.get('minimum-rate')) };
      if (purpose !== r.confirmed.purpose) config.processors = purpose === 'iq' ? [] : [{ id: `processor-${uid}`,
        kind: purpose === 'listen' ? 'demodulator' : 'decoder', mode: purpose === 'listen' ? 'nfm' : 'ft8', minimum_rate_hz: purpose === 'listen' ? 0 : 6_000 }];
      preview(pane, uid, config, true);
    });
    surface.body.addEventListener('invalid', event => {
      let parent = (event.target as HTMLElement).parentElement;
      while (parent && parent !== surface.body) { if (parent.tagName === 'DETAILS') (parent as HTMLDetailsElement).open = true; if (parent.hasAttribute('data-advanced-settings')) { parent.hidden = false; pane.expanded.add('advanced'); surface.body.querySelector('[data-action=advanced-settings]')?.setAttribute('aria-expanded', 'true'); } parent = parent.parentElement; }
    }, true);
    surface.body.addEventListener('input', event => {
      const input = event.target as HTMLInputElement, uid = session.resolve(pane.binding);
      const form = configForm(input);
      if (form) { syncAudioSettings(form); syncBandwidthSettings(form); pane.dirty = true; surface.body.querySelector('[data-preview]')?.replaceChildren(); rememberDraft(pane); draftStatus(pane, uid); }
      if (input.matches('[data-search]')) { search = input.value; renderPane(pane); }
      if (input.matches('[data-gain]')) { input.closest('.mc-monitor')!.querySelector('[data-gain-value]')!.textContent = `${Math.round(Number(input.value) * 100)}%`; session.monitor(uid, { gain: Number(input.value) }); }
      if (input.matches('[data-pan]')) { input.closest('.mc-monitor')!.querySelector('[data-pan-value]')!.textContent = panLabel(Number(input.value)); session.monitor(uid, { pan: Number(input.value) }); }
      if (input.matches('[data-master]')) { session.masterGain = Number(input.value); const output = surface.body.querySelector('[data-master-value]'); if (output) output.textContent = `${Math.round(session.masterGain * 100)}%`; session.notify(); }
    });
    surface.body.addEventListener('focusin', event => {
      if ((event.target as HTMLElement).matches('[data-channel-name]') && pane.tab !== 'receive') {
        pane.tab = 'receive'; saveViews(); renderPane(pane);
      }
    });
    surface.body.addEventListener('focusout', event => {
      if (!(event.target as HTMLElement).matches('input[type=range]')) return;
      setTimeout(() => { if (surface.root.isConnected) renderPane(pane); }, 0);
    });
    surface.body.addEventListener('change', event => {
      const input = event.target as HTMLSelectElement, uid = session.resolve(pane.binding);
      if (configForm(input)) { pane.dirty = true; surface.body.querySelector('[data-preview]')?.replaceChildren(); rememberDraft(pane); draftStatus(pane, uid); }
      if (!fixture && input.matches('[name=mode],[name=purpose]')) {
        const form = input.closest('form')!, modes = session.snapshot.capabilities.mode_descriptors ?? [];
        const select = form.querySelector<HTMLSelectElement>('[name=mode]')!, purpose = form.querySelector<HTMLSelectElement>('[name=purpose]');
        const processSettings = form.querySelector<HTMLElement>('[data-process-settings]'); if (processSettings) processSettings.hidden = !select.value;
        if (purpose && input.name === 'purpose' && purpose.value !== 'iq') select.value = modes.find(m => m.available && m.kind === (purpose?.value === 'listen' ? 'analog' : 'digital'))?.id ?? '';
        const mode = purpose?.value === 'iq' ? undefined : modes.find(m => m.id === select.value);
        const sharedAudio = form.querySelector<HTMLElement>('[data-shared-audio]');
        if (sharedAudio) sharedAudio.hidden = !mode || !(mode.outputs.includes('audio') || mode.audioCompanion !== null);
        const enhancements = form.querySelector<HTMLElement>('[data-monitor-enhancements]');
        if (enhancements) enhancements.hidden = !mode || ['drm', 'drmplus'].includes(mode.id);
        const remembered = session.modeMemory.get(session.resolve(pane.binding), mode?.id ?? '');
        if (remembered) {
          for (const [name, value] of Object.entries({ bandwidth: remembered.passband_hz, transition: remembered.transition_hz,
            'minimum-rate': remembered.minimum_rate_hz, 'processor-bandwidth': remembered.bandwidth_hz,
            'pcm-gain': remembered.pcm_gain, squelch: remembered.squelch_dbm,
            'nr-algorithm': remembered.nr_algorithm, 'nr-strength': remembered.nr_strength, 'nr-atten': remembered.nr_atten_db })) {
            const field = form.elements.namedItem(name) as HTMLInputElement | HTMLSelectElement | null; if (field) field.value = String(value);
          }
          const agc = form.querySelector<HTMLInputElement>('[name=agc]'); if (agc) agc.checked = remembered.agc;
          const nr = form.querySelector<HTMLInputElement>('[name=nr]'); if (nr) nr.checked = remembered.nr;
        } else if (mode) {
          if (purpose && input.name === 'mode') purpose.value = mode.kind === 'analog' ? 'listen' : 'digital';
          const bw = mode.defaultBandwidthHz, occupied = occupiedBandwidth(mode);
          const full = form.querySelector<HTMLInputElement>('[name=bandwidth]'), rate = form.querySelector<HTMLInputElement>('[name=minimum-rate]');
          if (full) full.value = String(Math.max(Number(full.value), occupied));
          if (rate) rate.value = String(Math.max(Number(rate.value), mode.minimumRateHz));
          const bandwidth = form.querySelector<HTMLInputElement>('[name=processor-bandwidth]'); if (bandwidth) bandwidth.value = String(bw);
          form.querySelector<HTMLInputElement>('[name=pcm-gain]')!.value = '0.5';
          form.querySelector<HTMLInputElement>('[name=agc]')!.checked = true;
          form.querySelector<HTMLInputElement>('[name=squelch]')!.value = '-160';
          form.querySelector<HTMLInputElement>('[name=nr]')!.checked = false;
          form.querySelector<HTMLSelectElement>('[name=nr-algorithm]')!.value = 'wiener';
          form.querySelector<HTMLInputElement>('[name=nr-strength]')!.value = '0.6';
          form.querySelector<HTMLInputElement>('[name=nr-atten]')!.value = '10';
        }
      }
      const form = configForm(input);
      if (form) { syncAudioSettings(form); syncBandwidthSettings(form); rememberDraft(pane); draftStatus(pane, uid); }
      if (input.matches('[data-monitor-mode]')) { session.monitorMode = input.value as 'manual' | 'follow'; input.blur(); session.notify(); }
      if (input.matches('[data-binding]')) { pane.binding = input.value === 'follow' ? { type: 'follow' } : { type: 'fixed', receiver_uid: input.value }; input.blur(); saveViews(); pane.window.sync(); renderPane(pane); }
    });
    renderPane(pane); saveViews();
  }
  function preview(pane: Pane, uid: string, config: ReceiverConfig, immediate = false): void {
    const r = receiver(uid); if (!r) return;
    if (session.edits.get(uid)?.pending || session.retiring(uid)) return;
    const service = session.snapshot.service_id, revision = r.revision;
    const command = { type: 'configure' as const, request_id: session.requestId(), receiver_uid: uid, expected_revision: r.revision, config };
    const impact = session.adapter.preflight(command), target = pane.window.body.querySelector('[data-preview]'); if (!target) return;
    // A preset replaces the candidate shown in the form as well as the preview.
    const form = pane.window.body.querySelector<HTMLFormElement>('[data-config]'), processor = config.processors[0];
    if (form) {
      const unit = form.querySelector<HTMLSelectElement>('[name=frequency-unit]')?.value ?? 'Hz';
      const values = { name: config.name, frequency: formatFrequencyValue(convertUnitValue(config.baseband.frequency_hz, 'Hz', unit), unit), purpose: config.purpose,
        bandwidth: config.baseband.passband_hz, transition: config.baseband.transition_hz, 'minimum-rate': config.baseband.minimum_rate_hz,
        mode: processor?.mode ?? '', 'processor-bandwidth': processor?.bandwidth_hz ?? 6000,
        'pcm-gain': processor?.pcm_gain ?? 0.5, squelch: processor?.squelch_dbm ?? -160,
        'nr-algorithm': processor?.nr_algorithm ?? 'wiener', 'nr-strength': processor?.nr_strength ?? 0.6, 'nr-atten': processor?.nr_atten_db ?? 10 };
      for (const [name, value] of Object.entries(values)) {
        const input = form.elements.namedItem(name) as HTMLInputElement | HTMLSelectElement | null;
        if (input) input.value = String(value);
      }
      const agc = form.querySelector<HTMLInputElement>('[name=agc]'); if (agc) agc.checked = processor?.agc !== false;
      const nr = form.querySelector<HTMLInputElement>('[name=nr]'); if (nr) nr.checked = processor?.nr ?? false;
      syncAudioSettings(form);
      syncBandwidthSettings(form);
      for (const [selector, hidden] of [['[data-process-settings]', !processor], ['[data-shared-audio]', !receiverHasAudio(session.snapshot, config)],
        ['[data-monitor-enhancements]', !processor || ['drm', 'drmplus'].includes(processor.mode)]] as const) {
        const field = form.querySelector<HTMLElement>(selector); if (field) field.hidden = hidden;
      }
    }
    pane.dirty = true; rememberDraft(pane); draftStatus(pane, uid);
    const submitted = JSON.stringify(pane.drafts.get(uid));
    const applyConfig = (): void => {
      if (service !== session.snapshot.service_id || receiver(uid)?.revision !== revision || session.edits.get(uid)?.pending || session.retiring(uid)) {
        target.textContent = l('Channel changed; review settings again.', '信道已变化，请重新检查设置后应用。'); return;
      }
      target.replaceChildren();
      void session.configure(uid, config).then(() => {
        if (disposed || service !== session.snapshot.service_id) return;
        if (!session.edits.get(uid)?.fault && JSON.stringify(pane.drafts.get(uid)) === submitted) {
          pane.drafts.delete(uid); pane.savedUid = uid;
          if (pane.renderedUid === uid) { pane.dirty = false; pane.renderedUid = undefined; }
        }
        renderPane(pane);
      });
    };
    if (!impact.accepted) {
      target.innerHTML = `<p class="mc-error" role="alert">${esc(impact.faults.map(f => faultText(f, config)).join('; '))}</p>`;
      return;
    }
    if (immediate && form?.dataset.revision === revision) { applyConfig(); return; }
    target.innerHTML = `<div class="mc-preview"><p>${l('Frequency', '频率')}: ${(r.confirmed.baseband.frequency_hz / 1e6).toFixed(6)} → ${(config.baseband.frequency_hz / 1e6).toFixed(6)} MHz<br>
      ${l('Demodulate / decode', '解调 / 解码')}: ${esc(r.confirmed.processors[0]?.mode.toUpperCase() ?? labels().iq)} → ${esc(config.processors[0]?.mode.toUpperCase() ?? labels().iq)}<br>
      ${impact.affected_tracks.length ? l('Active recording tracks may start a new segment when the signal settings change.', '信号设置变化时，活动录制轨道可能会建立新片段。') : l('No active recordings are affected.', '无活动录制受影响。')}<br>${esc(impact.faults.map(f => faultText(f, config)).join('; '))}</p></div>`;
    const apply = target.ownerDocument.createElement('button'); apply.type = 'button'; apply.className = 'mc-primary'; apply.textContent = l('Apply changes', '应用变更'); apply.title = l('Confirm these settings for this channel', '确认将这些设置应用到此信道');
    apply.addEventListener('click', applyConfig); target.appendChild(apply);
  }
  const render = (): void => {
    for (const [action, text] of Object.entries({ workbench: l('Channel workbench', '信道工作台'), recall: l('Recall all windows', '召回全部窗口'), reset: l('Reset layout', '重置布局'), 'quick-listen': l('＋ Listen to frequency', '＋ 监听频点') })) {
      const control = host.querySelector<HTMLElement>(`[data-action="${action}"]`); if (control) { control.textContent = text; control.title = workbenchTip(action); }
    }
    host.querySelector<HTMLElement>('[data-service-error]')!.textContent = session.fault ? `${session.fault.scope} · ${session.fault.code}: ${session.fault.message}` : '';
    host.querySelector<HTMLElement>('[data-capture]')!.textContent = captureSummary();
    const markers = host.querySelector<HTMLElement>('[data-markers]')!;
    patchWorkbench(markers, session.snapshot.receivers.filter(r => r.confirmed.enabled && session.snapshot.capture.usable_bandwidth_hz !== null).map(r =>
      `<button type="button" data-key="${esc(r.receiver_uid)}" title="${l('Select this channel for editing', '选中此信道以编辑设置')}" data-marker-uid="${esc(r.receiver_uid)}" style="left:${50 + (r.confirmed.baseband.frequency_hz - session.snapshot.capture.center_hz) / session.snapshot.capture.usable_bandwidth_hz! * 100}%;top:${r.display_number % 2 * 28 + 8}px;border-color:${esc(r.confirmed.color)}">${esc(`C${r.display_number} ${r.confirmed.name}${session.route(r.receiver_uid).enabled ? ` · ${l('Listen', '监听')}` : ''}`)}</button>`).join(''));
    panes.forEach(pane => renderPane(pane));
  };
  const recall = (reset = false): void => { panes.forEach(pane => pane.window.recall(reset)); archives.recall(reset); audioArchives?.recall(reset); library.recall(reset); };
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
      const fromPoint = session.snapshot.capture.center_hz + (Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)) - 0.5) * (session.snapshot.capture.usable_bandwidth_hz ?? 0);
      quickListen.open(marker ? receiver(marker.dataset.markerUid!)!.confirmed.baseband.frequency_hz : Math.round(fromPoint));
    });
  });
  host.addEventListener('click', event => {
    const marker = (event.target as HTMLElement).closest<HTMLElement>('[data-marker-uid]');
    if (marker) session.select(marker.dataset.markerUid!);
    const action = (event.target as HTMLElement).closest<HTMLElement>('[data-action]')?.dataset.action;
    if (action === 'quick-listen') quickListen.open(receiver(session.selected)?.confirmed.baseband.frequency_hz ?? session.snapshot.capture.center_hz);
    if (action === 'workbench') open('overview'); if (action === 'recall') recall(); if (action === 'reset') recall(true);
  });
  let storedViews: { kind: Pane['kind']; binding: ViewBinding; tab: string }[] = [];
  try { storedViews = JSON.parse(localStorage.getItem('websa-workbench-views-v1') ?? '[]'); } catch { /* invalid storage */ }
  open('overview');
  if (Array.isArray(storedViews)) for (const saved of storedViews.slice(0, 30)) {
    if (['detail', 'results'].includes(saved.kind) && saved.binding && (saved.binding.type === 'follow' || (saved.binding.type === 'fixed' && receiver(saved.binding.receiver_uid)))) { open(saved.kind, saved.binding); const pane = [...panes.values()][panes.size - 1]; if (['receive', 'processing', 'output', 'recording', 'results'].includes(saved.tab)) pane.tab = ['processing', 'results'].includes(saved.tab) ? 'receive' : saved.tab; renderPane(pane); }
  }
  const audioState = (): void => render(); document.addEventListener('websa:sdr-audio-gesture', audioState);
  const changedResults = new Set<string>();
  let resultsTimer: ReturnType<typeof setTimeout> | undefined;
  const unsubscribeResults = session.subscribeResults(uid => {
    changedResults.add(uid);
    if (resultsTimer !== undefined) return;
    resultsTimer = setTimeout(() => {
      resultsTimer = undefined;
      for (const pane of panes.values()) {
        const uid = session.resolve(pane.binding);
        if (!changedResults.has(uid) || pane.interacting || pane.window.root.hidden || pane.window.body.hidden || pane.window.root.ownerDocument.hidden) continue;
        const r = receiver(uid);
        if (pane.decode) updateDecode(pane.decode, uid, r);
        if (pane.inlineDecode) updateDecode(pane.inlineDecode.view, uid, r);
      }
      changedResults.clear();
    }, 50);
  });
  const unsubscribe = session.subscribe(render); const stopLang = onLangChange(() => { panes.forEach(p => p.window.sync()); render(); }); render();
  return { recall, showOverview: () => open('overview'), dispose: () => { document.removeEventListener('websa:sdr-audio-gesture', audioState); unsubscribeResults(); clearTimeout(resultsTimer); unsubscribe(); stopLang(); quickListen.dispose(); library.dispose(); archives.dispose(); disposed = true; audioArchives?.dispose(); panes.forEach(p => { p.unwireFrequency?.(); p.unwireFormats?.(); p.create?.dispose(); p.inlineDecode?.view.dispose(); p.presets?.dispose(); p.decode?.dispose(); p.window.dispose(); }); } };
}
