import type { RecordingTask } from '../core/multichannel';
import { getLang, onLangChange } from '../core/i18n';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import { recordingRange } from './recordingView';
import { readRecordingResponse } from './recordingResponse';
import { downloadResult } from './receiverExport';
import { mountRecordingDelete } from './recordingDelete';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const MAX_ARCHIVE = 16 * 1024 * 1024;

/** Only the bounded, exact server envelope is eligible for native media playback. */
export function recordedAudioWav(buffer: ArrayBuffer): Uint8Array<ArrayBuffer> {
  if (buffer.byteLength > MAX_ARCHIVE) throw new Error('Audio range exceeds 16 MiB');
  const bytes = new Uint8Array(buffer), decoder = new TextDecoder();
  const files = new Map<string, Uint8Array<ArrayBuffer>>();
  let at = 0;
  while (at + 512 <= bytes.length) {
    const header = bytes.subarray(at, at + 512);
    if (header.every(b => b === 0)) break;
    const text = (a: number, b: number): string => decoder.decode(header.subarray(a, b)).replace(/\0.*$/s, '').trim();
    const name = text(0, 100), sizeText = text(124, 136), checkText = text(148, 156);
    const limits: Record<string, number> = { 'selection.json': 2 * 1024 * 1024, 'frames.jsonl': 4 * 1024 * 1024, 'audio.wav': 8 * 1048576 + 58 };
    const checksum = header.reduce((n, b, i) => n + (i >= 148 && i < 156 ? 32 : b), 0);
    if (!Object.prototype.hasOwnProperty.call(limits, name) || files.has(name) || text(257, 263) !== 'ustar' || text(345, 500)
      || !['0', ''].includes(text(156, 157)) || !/^[0-7]+$/.test(sizeText) || !/^[0-7]+$/.test(checkText)
      || parseInt(checkText, 8) !== checksum) throw new Error('Invalid audio range archive');
    const size = parseInt(sizeText, 8), end = at + 512 + size;
    if (size > limits[name] || end > bytes.length) throw new Error('Truncated or oversized audio range');
    files.set(name, bytes.subarray(at + 512, end)); at += 512 + Math.ceil(size / 512) * 512;
  }
  if (files.size !== 3 || bytes.length - at < 1024 || bytes.subarray(at).some(b => b !== 0)) throw new Error('Incomplete audio range');
  const wav = files.get('audio.wav')!, f = new DataView(wav.buffer, wav.byteOffset, wav.byteLength);
  const label = (a: number, b: number): string => decoder.decode(wav.subarray(a, b));
  if (wav.length < 58 || label(0, 4) !== 'RIFF' || label(8, 16) !== 'WAVEfmt ' || f.getUint32(16, true) !== 18
    || f.getUint16(20, true) !== 3 || f.getUint16(34, true) !== 32 || f.getUint16(36, true) !== 0
    || label(38, 42) !== 'fact' || f.getUint32(42, true) !== 4 || label(50, 54) !== 'data') throw new Error('Unexpected recorded audio format');
  const channels = f.getUint16(22, true), rate = f.getUint32(24, true), frames = f.getUint32(46, true), stride = channels * 4;
  const manifest = JSON.parse(decoder.decode(files.get('selection.json')));
  if (![1, 2].includes(channels) || rate < 8000 || rate > 192000 || frames < 1 || frames > 1048576 || frames / rate > 60
    || f.getUint32(4, true) + 8 !== wav.length || f.getUint32(54, true) !== frames * stride || wav.length !== 58 + frames * stride
    || f.getUint16(32, true) !== stride || f.getUint32(28, true) !== rate * stride
    || manifest.format !== 'websa-audio-range-v1' || manifest.audio?.source !== 'published_audio'
    || manifest.audio.rate_hz !== rate || manifest.audio.channels !== channels || manifest.audio.frames !== String(frames)) throw new Error('Inconsistent audio range');
  return wav;
}

/** One original-PCM player per workbench, separate from the live monitoring bus. */
export function mountRecordingAudioView(session?: WorkbenchSession): { open(task: RecordingTask): void; recall(reset?: boolean): void; dispose(): void } {
  let current: { task: string; window: FloatingWindow; dispose(): void } | null = null;
  return {
    open(task) {
      if (session && !session.snapshot.recordings.some(t => t.task_id === task.task_id)) return;
      if (current?.task === task.task_id) { current.window.show(); if (current.window.portal.isPoppedOut()) current.window.portal.popout(); return; }
      current?.dispose(); current = null;
      const window = createFloatingWindow('recorded-audio', () => l('Recorded audio', '已录音频'), { x: 140, y: 110, w: 620, h: 430 });
      const body = window.body; body.dataset.recordedAudio = '';
      body.innerHTML = `<details><summary data-label="source"></summary><p data-owner class="mc-clock"></p></details><p data-prefix role="status"></p>
        <form class="mc-fields"><label><span data-label="segment"></span><select name="segment"></select></label>
        <label><span data-label="start"></span><input name="start" value="0" inputmode="numeric" required></label>
        <label><span data-label="count"></span><input name="count" inputmode="numeric" required></label>
        <div class="mc-toolbar"><button type="submit" data-audio-load></button><button type="button" data-audio-cancel hidden></button><button type="button" data-audio-save disabled></button></div></form>
        <p data-audio-info class="mc-clock"></p><p data-audio-status role="status"></p><audio controls preload="metadata" style="width:100%"></audio>`;
      const get = <T extends HTMLElement>(s: string): T => body.querySelector<T>(s)!;
      const select = get<HTMLSelectElement>('select'), start = get<HTMLInputElement>('[name=start]'), count = get<HTMLInputElement>('[name=count]');
      const load = get<HTMLButtonElement>('[data-audio-load]'), save = get<HTMLButtonElement>('[data-audio-save]'), cancel = get<HTMLButtonElement>('[data-audio-cancel]');
      const status = get('[data-audio-status]'), audio = get<HTMLAudioElement>('audio');
      const segments = task.tracks.find(t => t.kind === 'audio')?.segments.filter(s => s.segment_id && BigInt(s.samples) > 0n) ?? [];
      segments.forEach((s, i) => { const option = body.ownerDocument.createElement('option'); option.value = String(i); option.textContent = `${i + 1} · ${s.actual_rate_hz} Hz · ${s.channels} ch · ${s.samples}`; select.append(option); });
      get('[data-owner]').textContent = `${task.task_id} · ${task.service_id} · ${task.receiver_uid}`;
      let controller: AbortController | null = null, url: string | null = null, archive: Blob | null = null, disposed = false;
      const clear = (): void => {
        controller?.abort(); controller = null;
        if (audio.hasAttribute('src')) { audio.pause(); audio.removeAttribute('src'); audio.load(); }
        if (url) URL.revokeObjectURL(url); url = null; archive = null;
        save.disabled = true; load.disabled = !segments.length; cancel.hidden = true; status.textContent = '';
      };
      const labels = (): void => {
        for (const [key, text] of Object.entries({ source: l('Source', '来源'), segment: l('Segment', '片段'), start: l('Start / sample frame', '起点 / 样本帧'), count: l('Length / sample frames', '长度 / 样本帧') })) get(`[data-label="${key}"]`).textContent = text;
        get('[data-prefix]').textContent = task.state === 'failed' ? `${l('Verified prefix only', '仅已校验前缀')}${task.fault ? ` · ${task.fault.message}` : ''}` : '';
        load.textContent = l('Load audio', '载入音频'); save.textContent = l('Download audio + source', '下载音频与来源'); cancel.textContent = l('Cancel', '取消');
        load.title = l('Saved audio · at most 60 seconds / 1,048,576 frames · live reception is independent', '已保存音频 · 最多 60 秒 / 1,048,576 样本帧 · 与实时接收独立');
        save.title = l('Download this exact Float32 WAV snapshot and original frame clocks', '下载同一份 Float32 WAV 快照及原始帧时钟');
        audio.setAttribute('aria-label', l('Recorded audio playback', '已录音频试听'));
        const s = segments[Number(select.value)];
        get('[data-audio-info]').textContent = s ? `${s.actual_rate_hz} Hz · ${s.channels} ch · ${s.samples} ${l('sample frames', '样本帧')}` : l('No saved audio samples', '无已保存音频样本');
        if (archive) status.textContent = l('Ready', '就绪'); else if (controller) status.textContent = l('Loading…', '正在载入…');
      };
      const defaults = (): void => { const s = segments[Number(select.value)]; start.value = '0'; count.value = s ? String(Math.min(Number(BigInt(s.samples) < 1048576n ? BigInt(s.samples) : 1048576n), Math.floor((s.actual_rate_hz ?? 0) * 60))) : '0'; labels(); };
      body.addEventListener('input', e => { if (!(e.target as HTMLElement).closest('audio')) clear(); });
      body.addEventListener('change', e => { if (!(e.target as HTMLElement).closest('audio')) { clear(); if (e.target === select) defaults(); } });
      get<HTMLFormElement>('form').addEventListener('submit', e => {
        e.preventDefault(); clear(); const request = new AbortController(); controller = request;
        void (async () => {
          try {
            const s = segments[Number(select.value)]; if (!s) throw new Error(l('No saved audio samples', '无已保存音频样本'));
            const query = recordingRange(s.samples, start.value, count.value);
            if (Number(query.get('count')) / (s.actual_rate_hz ?? 0) > 60) throw new Error(l('Select at most 60 seconds', '请选择最多 60 秒'));
            query.set('segment', s.segment_id!); load.disabled = true; cancel.hidden = false; labels();
            const response = await fetch(`/api/sdr/recordings/${encodeURIComponent(task.task_id)}/audio/range?${query}`, { signal: request.signal });
            if (!response.ok) { const error = await response.json(); throw new Error(error.error ?? `HTTP ${response.status}`); }
            const data = await readRecordingResponse(response, 1024, MAX_ARCHIVE);
            if (disposed || controller !== request || request.signal.aborted) return;
            const wav = recordedAudioWav(data);
            archive = new Blob([data], { type: 'application/x-tar' }); url = URL.createObjectURL(new Blob([wav], { type: 'audio/wav' })); audio.src = url;
            save.disabled = false; labels();
          } catch (error) {
            if (!disposed && controller === request && !request.signal.aborted) status.textContent = error instanceof Error ? error.message : String(error);
          } finally { request.abort(); if (!disposed && controller === request) { controller = null; load.disabled = false; cancel.hidden = true; } }
        })();
      });
      cancel.addEventListener('click', clear);
      save.addEventListener('click', () => { if (archive) downloadResult(body.ownerDocument, archive, `${task.task_id}-audio-range.tar`); });
      const observer = new MutationObserver(() => { if (body.hidden || window.root.hidden) clear(); });
      observer.observe(body, { attributes: true, attributeFilter: ['hidden'] }); observer.observe(window.root, { attributes: true, attributeFilter: ['hidden'] });
      const unsubscribe = onLangChange(labels); defaults(); clear(); window.show();
      let disposeDelete: (() => void) | undefined;
      current = { task: task.task_id, window, dispose: () => { disposed = true; disposeDelete?.(); clear(); observer.disconnect(); unsubscribe(); window.dispose(); } };
      if (session) disposeDelete = mountRecordingDelete(body, session, task, () => { current?.dispose(); current = null; });
    },
    recall(reset) { current?.window.recall(reset); },
    dispose() { current?.dispose(); current = null; },
  };
}
