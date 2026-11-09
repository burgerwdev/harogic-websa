import { getLang } from '../core/i18n';
import { clone, receiverHasAudio, type RecordingTask, type TrackKind } from '../core/multichannel';
import type { DecoderLogEntry } from '../sdr/decoderLog';
import type { WorkbenchSession } from './workbenchSession';
import { liveStatus } from './receiverLive';
import { ReceiverMeter, receiverLevel, receiverLevelScale } from './receiverLevel';
import { mountReceiverTicker, type ReceiverTicker } from './receiverTicker';
import { createWorkbenchPatcher } from './workbenchDom';
import { setDomAttribute, setDomProperty } from './domState';
import { workbenchIcon as icon } from './workbenchIcons';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const esc = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);

export interface ChannelStripReadouts {
  results(uid: string): { entries: readonly DecoderLogEntry[]; evicted: number; source?: string | null };
  playback?(uid: string): Record<string, string | number | boolean> | undefined;
  waveform?(uid: string, samples: Float32Array<ArrayBuffer> | null): boolean;
}
interface ChannelStripOptions {
  receiverUid(): string;
  available(command: string): boolean;
  recording(uid: string): { kind: TrackKind; task?: RecordingTask; pending: boolean } | undefined;
  readouts?: ChannelStripReadouts;
  current?(): boolean;
  matchesCurrent?(): boolean;
}
export interface WorkbenchChannelStrip {
  update(): void;
  updateMeter(): void;
  reset(): void;
  dispose(): void;
}

/** Owns only this window's compact view and edit draft; confirmed state stays in the session. */
export function mountWorkbenchChannelStrip(root: HTMLElement, session: WorkbenchSession, options: ChannelStripOptions): WorkbenchChannelStrip {
  const strip = root.ownerDocument.createElement('div'); strip.className = 'mc-channel-strip';
  const patch = createWorkbenchPatcher(strip);
  root.querySelector('.mc-head strong')!.after(strip);
  const fixture = session.snapshot.transport === 'fixture';
  const { available, readouts: native } = options;
  const service = (): string => session.snapshot.service_id ?? 'fixture';
  const receiver = (uid: string) => session.snapshot.receivers.find(r => r.receiver_uid === uid);
  let ticker: ReceiverTicker | undefined, meterState = new ReceiverMeter(), volume = false, disposed = false;
  let rename: { uid: string; service: string; value: string } | undefined;
  const currentOwner = (): string | undefined => strip.dataset.serviceId === service() && receiver(strip.dataset.receiverUid ?? '')
    ? strip.dataset.receiverUid : undefined;
  function updateMeter(): void {
    if (disposed) return;
    const meter = strip.querySelector<HTMLElement>('[data-strip-meter]'); if (!meter) return;
    const uid = currentOwner(), r = uid ? receiver(uid) : undefined;
    const result = uid ? native?.results(uid) : undefined;
    const reading = r ? receiverLevel(r, result?.entries ?? [], native?.playback?.(uid!), result?.source) : null;
    const identity = `${session.snapshot.service_id}:${uid}:${r?.stream?.source_id}:${r?.stream?.epoch}:${r?.stream?.channel_id}:${reading?.source}`;
    const display = meterState.update(reading, identity);
    const scale = receiverLevelScale(reading ? { ...reading, db: display.level } : null);
    const peak = receiverLevelScale(reading ? { ...reading, db: display.peak } : null);
    const label = reading ? `${reading.source === 'playback' ? l('Playing audio (before monitor volume)', '播放音频（监听音量之前）') : reading.source === 'audio' ? l('Demodulated audio (not playing)', '解调音频（未播放）') : l('RF input', '射频输入')} · ${reading.source === 'playback' ? 'RMS ' : ''}${reading.db.toFixed(1)} dBFS${reading.peakDb !== undefined ? ` · ${l('Peak', '峰值')} ${reading.peakDb.toFixed(1)} dBFS` : ''}${display.clipped ? ` · ${l('CLIP', '削波')}` : ''}` : l('No live level', '暂无实时电平');
    setDomProperty(meter.dataset, 'available', String(!!reading)); setDomProperty(meter.dataset, 'clipped', String(display.clipped));
    setDomProperty(meter, 'title', label); setDomAttribute(meter, 'aria-label', label); setDomAttribute(meter, 'aria-valuetext', label);
    setDomAttribute(meter, 'aria-valuemin', String(scale.min)); setDomAttribute(meter, 'aria-valuenow', String(scale.value));
    meter.style.setProperty('--level', `${scale.percent}%`); meter.style.setProperty('--peak', `${peak.percent}%`);
    setDomProperty(meter.querySelector('small')!, 'textContent', reading ? `${reading.db.toFixed(0)} dBFS` : '— dBFS');
  }
  function update(): void {
    if (disposed) return;
    const uid = options.receiverUid(), r = receiver(uid);
    if (!r) { reset(); ticker?.dispose(); ticker = undefined; strip.textContent = l('Channel deleted', '信道已删除'); return; }
    if (strip.dataset.receiverUid !== uid || strip.dataset.serviceId !== service()) reset();
    strip.dataset.receiverUid = uid; strip.dataset.serviceId = service();
    const audio = receiverHasAudio(session.snapshot, r.confirmed), monitor = audio && (fixture || !!session.snapshot.capabilities.monitor_mixing);
    const recording = options.recording(uid), kind = recording?.kind, task = recording?.task;
    const reception = liveStatus(r), state = reception.kind === 'healthy' ? 'running' : reception.kind === 'fault' ? 'failed' : reception.kind === 'stopped' ? 'stopped' : 'interrupted';
    const result = native?.results(uid), entries = result?.entries ?? [];
    const control = (action: string, glyph: string, label: string, extra = ''): string => `<button type="button" data-action="${action}" aria-label="${esc(label)}" title="${esc(label)}" ${extra}>${icon(glyph)}</button>`;
    const route = session.route(uid);
    const temporary = options.current?.() ?? false, name = temporary ? 'Current' : r.confirmed.name;
    strip.dataset.frequencyMatch = String(options.matchesCurrent?.() ?? false);
    const recordLabel = kind === 'audio' ? l('audio', '音频') : kind === 'results' ? l('results', '结果') : 'IQ';
    patch(`<div class="mc-strip-identity" title="${esc(`${name} · ${reception.label}`)}"><span><i class="mc-state-dot" data-state="${state}"></i>${temporary ? '<b>Current</b>' : rename ? `<input data-strip-name-edit data-key="strip-name-${esc(uid)}" data-no-drag aria-label="${l('Channel name', '信道名称')}" value="${esc(rename.value)}" maxlength="128">` : `<b data-strip-name data-no-drag tabindex="0" role="button" title="${l('Double-click to rename; Enter to save, Escape to cancel', '双击改名；回车提交，Esc 取消')}">${esc(name)}</b>`}</span><small>${(r.confirmed.baseband.frequency_hz / 1e6).toFixed(6)} <span>MHz</span>${options.matchesCurrent?.() ? `<span class="mc-strip-current" title="${l('Matches Current frequency', '与 Current 调谐频率一致')}">${icon('crosshair')}Current</span>` : ''}</small></div>
      <div data-strip-text data-key="strip-text"></div>
      <div class="mc-strip-level" data-strip-meter data-no-drag role="meter" aria-valuemax="0"><span class="mc-strip-level-fill"></span><span class="mc-strip-level-peak"></span><small></small></div>
      <div class="mc-strip-actions">${monitor ? control('listen', 'headphones', session.listening(uid) ? l('Stop listening', '停止监听') : l('Listen', '监听'), `aria-pressed="${session.listening(uid)}" ${r.state !== 'running' || r.fault ? 'disabled' : ''}`) : ''}
      ${kind && available('record_start') ? control(`rec-${kind}`, 'record', task ? l(`Stop ${recordLabel} recording`, `停止${recordLabel}录制`) : l(`Record ${recordLabel}`, `录制${recordLabel}`), `aria-pressed="${!!task}" class="${task ? 'mc-recording-on' : ''}" ${recording?.pending || session.retiring(uid) || task?.state === 'finalizing' || !task && (session.snapshot.recording_resources?.available_tasks === 0 || r.state !== 'running') ? 'disabled' : ''}`) : ''}
      ${monitor ? `<div class="mc-strip-volume">${control('strip-volume', route.gain ? 'volume' : 'muted', l('Channel volume', '信道音量'), `aria-expanded="${volume}"`)}<div class="mc-strip-volume-panel" data-no-drag ${volume ? '' : 'hidden'}><label>${l('Volume', '音量')}<input data-strip-gain data-key="strip-gain-${esc(uid)}" type="range" min="0" max="1" step="0.01" value="${route.gain}" aria-label="${l('Channel volume', '信道音量')}"></label><output data-strip-volume-value>${Math.round(route.gain * 100)}%</output></div></div>` : ''}
      ${temporary ? control('save-current', 'plus', l('Save current channel', '保存当前信道')) : available('retire') ? control('retire', 'trash', session.retiring(uid) ? l('Deleting…', '正在删除…') : l('Delete channel', '删除信道'), `class="mc-strip-delete" ${session.retiring(uid) || session.edits.get(uid)?.pending ? 'disabled' : ''}`) : ''}</div>`, element => element.hasAttribute('data-strip-text') || element.hasAttribute('data-strip-meter') || element === strip.ownerDocument.activeElement && element.matches('input'));
    updateMeter();
    ticker ??= mountReceiverTicker(strip.querySelector<HTMLElement>('[data-strip-text]')!, root);
    ticker.update(entries, r, session.snapshot.service_id ?? 'fixture', result?.source);
  }

  function reset(): void {
    patch.reset();
    rename = undefined; volume = false; meterState = new ReceiverMeter();
  }
  const beginRename = (): void => {
    const uid = currentOwner(), r = uid ? receiver(uid) : undefined;
    if (!uid || !r || options.current?.() || uid !== options.receiverUid() || session.retiring(uid) || session.edits.get(uid)?.pending || !available('configure')) return;
    rename = { uid, service: service(), value: r.confirmed.name };
    update();
    const input = strip.querySelector<HTMLInputElement>('[data-strip-name-edit]'); input?.focus(); input?.select();
  };
  const finishRename = (submit: boolean): void => {
    const draft = rename; if (!draft) return;
    const r = receiver(draft.uid), name = draft.value.trim();
    if (submit && (!name || /[\u0000-\u001f\u007f]/.test(name))) {
      const input = strip.querySelector<HTMLInputElement>('[data-strip-name-edit]');
      input?.setCustomValidity(l('Enter a channel name without control characters.', '请输入不含控制字符的信道名称。')); input?.reportValidity(); return;
    }
    rename = undefined;
    if (submit && r && draft.uid === options.receiverUid() && draft.service === service() && !session.retiring(draft.uid) && available('configure') && name !== r.confirmed.name)
      void session.configure(draft.uid, { ...clone(session.edits.get(draft.uid)?.desired ?? r.confirmed), name });
    update();
  };
  const doubleClick = (event: MouseEvent): void => {
    if (!(event.target as HTMLElement).closest('[data-strip-name]')) return;
    event.preventDefault(); event.stopPropagation(); beginRename();
  };
  const keyDown = (event: KeyboardEvent): void => {
    const target = event.target as HTMLElement;
    const consume = (): void => { event.preventDefault(); event.stopImmediatePropagation(); };
    if (target.matches('[data-strip-name]') && ['Enter', 'F2'].includes(event.key)) { consume(); beginRename(); return; }
    if (target.matches('[data-strip-name-edit]') && ['Enter', 'Escape'].includes(event.key) && !event.isComposing) {
      consume(); finishRename(event.key === 'Enter');
      if (!rename) strip.querySelector<HTMLElement>('[data-strip-name]')?.focus(); return;
    }
    if (event.key === 'Escape' && volume) {
      consume(); volume = false; update(); strip.querySelector<HTMLButtonElement>('[data-action=strip-volume]')?.focus();
    }
  };
  const click = (event: MouseEvent): void => {
    if (!(event.target as HTMLElement).closest('[data-action=strip-volume]')) return;
    event.preventDefault(); event.stopPropagation();
    const uid = currentOwner();
    if (!uid || session.retiring(uid)) { update(); return; }
    volume = !volume; update();
    if (volume) strip.querySelector<HTMLInputElement>('[data-strip-gain]')?.focus();
  };
  const input = (event: Event): void => {
    const field = event.target as HTMLInputElement;
    if (field.matches('[data-strip-name-edit]') && rename) { rename.value = field.value; field.setCustomValidity(''); }
    if (!field.matches('[data-strip-gain]')) return;
    const uid = currentOwner(); if (!uid || session.retiring(uid)) return;
    const output = strip.querySelector('[data-strip-volume-value]'); if (output) output.textContent = `${Math.round(Number(field.value) * 100)}%`;
    session.monitor(uid, { gain: Number(field.value) });
  };
  const focusOut = (event: FocusEvent): void => {
    if ((event.target as HTMLElement).matches('[data-strip-name-edit]')) finishRename(false);
  };
  strip.addEventListener('dblclick', doubleClick);
  strip.addEventListener('click', click);
  strip.addEventListener('input', input);
  strip.addEventListener('focusout', focusOut);
  root.addEventListener('keydown', keyDown, true);
  return { update, updateMeter, reset, dispose: () => {
    if (disposed) return;
    disposed = true; reset(); ticker?.dispose();
    strip.removeEventListener('dblclick', doubleClick);
    strip.removeEventListener('click', click);
    strip.removeEventListener('input', input);
    strip.removeEventListener('focusout', focusOut);
    root.removeEventListener('keydown', keyDown, true);
    strip.remove();
  } };
}
