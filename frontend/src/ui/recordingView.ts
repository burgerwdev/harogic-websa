import type { RecordingTask } from '../core/multichannel';
import { appendCopyDetails } from './copyDetails';
import { getLang, onLangChange } from '../core/i18n';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import { downloadResult } from './receiverExport';
import { mountRecordingDemod } from './recordingDemod';
import { mountRecordingDelete } from './recordingDelete';
import { readRecordingResponse } from './recordingResponse';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const MAX_SAMPLES = 1_048_576n;
// 8 MiB CF32 + 4 MiB source metadata + tar headers/padding.
const MAX_RANGE_ARCHIVE = 16 * 1024 * 1024;

/** Offsets are relative to one recorded segment, never JS approximations of source u64s. */
export function recordingRange(total: string, start: string, count: string, limit = MAX_SAMPLES): URLSearchParams {
  if (![total, start, count].every(value => /^\d+$/.test(value))) throw new Error(l('Use whole sample counts', '请输入整数样本数'));
  const offset = BigInt(start), length = BigInt(count);
  if (length < 1n || length > limit || offset + length > BigInt(total)) {
    throw new Error(l(`Choose 1–${limit.toLocaleString()} samples within this segment`, `请在该片段内选择 1–${limit.toLocaleString()} 个样本`));
  }
  return new URLSearchParams({ start, count });
}

/** One bounded archive buffer for the whole workbench, independent of live consumers. */
export function mountRecordingView(options: { session?: WorkbenchSession; demodulateOffset?: () => boolean; demodulateModes?: () => string[]; collection?: () => boolean; streaming?: () => boolean; exportLimit?: () => string } = {}): { open(task: RecordingTask): void; recall(reset?: boolean): void; dispose(): void } {
  let current: { task: string; window: FloatingWindow; dispose(): void } | null = null;
  return {
    open(task) {
      if (options.session && !options.session.snapshot.recordings.some(t => t.task_id === task.task_id)) return;
      if (current?.task === task.task_id) { current.window.show(); if (current.window.portal.isPoppedOut()) current.window.portal.popout(); return; }
      current?.dispose();
      const surface = createFloatingWindow('iq-recording', () => l('Recorded IQ · waveform / export', '已录 IQ · 波形回放与导出'), { x: 140, y: 170, w: 680, h: 540 });
      const body = surface.body;
      // Static subtree: polling never patches these controls or the loaded waveform.
      body.innerHTML = `<p data-recording-title class="mc-clock"></p><p data-task-fault class="mc-error" role="alert" hidden></p>
        <details><summary data-identity-label></summary><p data-owner class="mc-clock"></p><p data-boundary class="mc-notice"></p></details>
        <form data-range-form class="mc-fields"><label><span data-label="segment"></span><select name="segment"></select></label>
        <label><span data-label="start"></span><input name="start" inputmode="numeric" value="0" required></label>
        <label><span data-label="count"></span><input name="count" inputmode="numeric" required></label>
        <div class="mc-toolbar"><button type="submit" data-load></button><button type="button" data-download></button><button type="button" data-download-full hidden></button><button type="button" data-download-collection hidden></button></div></form>
        <p data-info class="mc-clock"></p><p data-status role="status"></p>
        <canvas data-iq-wave width="1000" height="200" style="width:100%;height:150px" aria-label="IQ waveform"></canvas>
        <div class="mc-toolbar"><button type="button" data-play disabled></button><label style="flex:1"><span data-label="position"></span>
        <input data-position type="range" min="0" max="0" value="0" style="width:100%" disabled></label></div><p data-position-info class="mc-clock"></p>`;
      const get = <T extends HTMLElement>(selector: string): T => body.querySelector<T>(selector)!;
      const segmentSelect = get<HTMLSelectElement>('[name=segment]');
      const segments = task.tracks[0].segments.filter(s => s.segment_id && BigInt(s.samples) > 0n);
      for (const [index, segment] of segments.entries()) {
        const option = document.createElement('option'); option.value = String(index);
        option.textContent = `${index + 1} · ${((segment.frequency_hz ?? 0) / 1e6).toFixed(6)} MHz · ${((segment.actual_rate_hz ?? 0) / 1000).toFixed(3)} kS/s`;
        option.title = `${segment.actual_rate_hz} S/s · ${segment.samples} samples`;
        segmentSelect.append(option);
      }
      get('[data-owner]').textContent = `${task.task_id} · ${task.service_id} · ${task.receiver_uid}`;
      appendCopyDetails(get('details'), () => task);
      const start = get<HTMLInputElement>('[name=start]'), count = get<HTMLInputElement>('[name=count]');
      const position = get<HTMLInputElement>('[data-position]'), play = get<HTMLButtonElement>('[data-play]');
      let abort: AbortController | null = null, disposed = false, playing = false, timer: number | undefined;
      let samples: DataView | null = null, loadedStart = '', loadedRate = 1, busy = false;
      const labels = (): void => {
        surface.sync();
        const created = Number(task.created_at_ms), date = new Date(created);
        get('[data-recording-title]').textContent = (task.state === 'failed' ? l('Failed IQ recording', '失败的 IQ 录制') : l('Completed IQ recording', '已完成的 IQ 录制'))
          + (Number.isSafeInteger(created) && Number.isFinite(date.getTime()) ? ` · ${date.toLocaleString(getLang() === 'zh' ? 'zh-CN' : 'en-GB')}` : '');
        get('[data-identity-label]').textContent = l('Recording source details', '录制来源详情');
        get('[data-task-fault]').hidden = task.state !== 'failed';
        get('[data-task-fault]').textContent = task.state === 'failed' ? l('Task failed · verified saved prefix only. ', '任务失败，仅查看已校验的保存前缀。') + (task.fault?.message ?? '') : '';
        const texts: Record<string, string> = { segment: l('Recorded segment', '已录片段'), start: l('Start / samples', '起点 / 样本'),
          count: l('Length / samples', '长度 / 样本'), position: l('Playback position', '回放位置') };
        for (const [key, text] of Object.entries(texts)) get(`[data-label=${key}]`).textContent = text;
        get('[data-boundary]').textContent = l('CF32 LE · SigMF 1.2.6 · waveform preview ≤ 1,048,576 samples', 'CF32 LE · SigMF 1.2.6 · 波形预览 ≤ 1,048,576 样本');
        get('[data-load]').textContent = l('Load waveform', '载入波形');
        get('[data-download]').textContent = l('Download SigMF', '下载 SigMF');
        get('[data-download-full]').textContent = l('Entire segment', '整段下载');
        get('[data-download-full]').hidden = !options.streaming?.();
        get('[data-download-collection]').hidden = !options.collection?.() || segments.length < 2;
        get('[data-download-collection]').textContent = l(`All segments (${segments.length})`, `全部片段（${segments.length}）`);
        get('[data-download-collection]').title = l('Download this task as a SigMF Collection; each segment keeps its own rate, frequency and source clock', '将本任务下载为 SigMF 集合，各片段保留独立采样率、频率和源时钟');
        get('[data-download-full]').title = l('Download all samples in this segment as SigMF', '将此片段的全部样本下载为 SigMF');
        get('[data-load]').title = l('Read and verify the selected samples from server storage', '从服务器存储读取并校验所选样本');
        get('[data-download]').title = l('Save this range as a standard SigMF archive', '将此范围保存为标准 SigMF 归档');
        play.textContent = playing ? l('Pause', '暂停') : l('Play waveform', '播放波形');
        play.title = l('Advance through the recorded I/Q waveform at its actual sample rate', '按真实采样率推进已录 I/Q 波形');
        position.title = l('Seek within the loaded range without tuning live reception', '在已载入范围内定位，不改变实时调谐');
        start.title = l('Zero-based sample offset within the selected recorded segment', '所选已录片段内从零计数的样本偏移');
        count.title = l('Number of complex samples to read or export', '要读取或导出的复数样本数');
        segmentSelect.title = l('Keep each source configuration and sample rate separate', '按真实源配置和采样率分别选择片段');
      };
      const draw = (): void => {
        const canvas = get<HTMLCanvasElement>('[data-iq-wave]'), ctx = canvas.getContext('2d'); if (!ctx) return;
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        if (!samples) return;
        const length = samples.byteLength / 8, at = Number(position.value), first = Math.min(at, Math.max(0, length - 1024)), visible = Math.min(1024, length - first);
        let peak = 0;
        for (let n = first; n < first + visible; n++) for (let component = 0; component < 2; component++) {
          const value = samples.getFloat32(n * 8 + component * 4, true); if (Number.isFinite(value)) peak = Math.max(peak, Math.abs(value));
        }
        for (let component = 0; component < 2; component++) {
          ctx.strokeStyle = component ? '#eaa64b' : '#46bb8c'; ctx.beginPath();
          for (let n = 0; n < visible; n++) {
            const value = samples.getFloat32((first + n) * 8 + component * 4, true);
            const x = n / Math.max(1, visible - 1) * canvas.width, y = canvas.height / 2 - (Number.isFinite(value) ? value / (peak || 1) : 0) * canvas.height * 0.46;
            if (n) ctx.lineTo(x, y); else ctx.moveTo(x, y);
          }
          ctx.stroke();
        }
        ctx.strokeStyle = '#888'; ctx.beginPath();
        const cursor = (at - first) / Math.max(1, visible - 1) * canvas.width;
        ctx.moveTo(cursor, 0); ctx.lineTo(cursor, canvas.height); ctx.stroke();
        get('[data-position-info]').textContent = `I / Q · ${l('sample', '样本')} ${BigInt(loadedStart) + BigInt(at)} · ${(at / loadedRate).toFixed(3)} s · ${l('display scale', '显示比例')} ±${peak.toPrecision(4)}`;
      };
      const pause = (): void => { playing = false; if (timer !== undefined) window.clearInterval(timer); timer = undefined; labels(); };
      const clear = (): void => { pause(); samples = null; position.disabled = true; play.disabled = true; position.value = '0'; draw(); get('[data-info]').textContent = ''; get('[data-position-info]').textContent = ''; };
      const defaults = (): void => {
        clear(); const segment = segments[Number(segmentSelect.value)]; start.value = '0';
        count.value = segment ? (BigInt(segment.samples) > MAX_SAMPLES ? MAX_SAMPLES : BigInt(segment.samples)).toString() : '0';
      };
      const request = async (format: string): Promise<Response> => {
        const segment = segments[Number(segmentSelect.value)]; if (!segment) throw new Error(l('No saved samples', '没有已保存样本'));
        const query = recordingRange(segment.samples, start.value, count.value); query.set('segment', segment.segment_id!); query.set('format', format);
        abort = new AbortController();
        const response = await fetch(`/api/sdr/recordings/${encodeURIComponent(task.task_id)}/range?${query}`, { signal: abort.signal });
        if (!response.ok) { const error = await response.json(); throw new Error(error.error ?? `HTTP ${response.status}`); }
        return response;
      };
      const nativeDownload = (endpoint: string, filename: string): void => {
        const anchor = body.ownerDocument.createElement('a');
        anchor.href = `/api/sdr/recordings/${encodeURIComponent(task.task_id)}/${endpoint}`;
        anchor.download = filename; anchor.target = '_blank'; anchor.rel = 'noopener'; anchor.hidden = true;
        body.ownerDocument.body.append(anchor); anchor.click(); anchor.remove();
        get('[data-status]').textContent = l('Download requested', '已请求下载');
      };
      const streamDownload = (full = false): void => {
        try {
          const segment = segments[Number(segmentSelect.value)]; if (!segment) throw new Error(l('No saved samples', '没有已保存样本'));
          const offset = full ? '0' : start.value, length = full ? segment.samples : count.value;
          const query = recordingRange(segment.samples, offset, length, BigInt(options.exportLimit?.() ?? '1048576'));
          query.set('segment', segment.segment_id!);
          nativeDownload(`export?${query}`, `${task.task_id}-${offset}-${length}.sigmf`);
        } catch (error) { get('[data-status]').textContent = String(error instanceof Error ? error.message : error); }
      };
      const run = async (download: boolean): Promise<void> => {
        if (busy) return;
        if (download && options.streaming?.()) { pause(); streamDownload(); return; }
        busy = true; pause();
        body.querySelectorAll<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>('form input,form button,form select').forEach(el => { el.disabled = true; });
        get('[data-status]').textContent = l('Reading and verifying…', '正在读取并校验…');
        try {
          if (download) {
            const response = await request('sigmf'), bytes = await readRecordingResponse(response, 1024, MAX_RANGE_ARCHIVE); if (disposed) return;
            const blob = new Blob([bytes], { type: 'application/x-tar' });
            downloadResult(body.ownerDocument, blob, `${task.task_id}-${segmentSelect.value}-${start.value}-${count.value}.sigmf`);
          } else {
            clear(); const expectedBytes = Number(count.value) * 8;
            const response = await request('cf32'), bytes = await readRecordingResponse(response, expectedBytes, expectedBytes); if (disposed) return;
            samples = new DataView(bytes); loadedStart = start.value; loadedRate = segments[Number(segmentSelect.value)].actual_rate_hz!;
            position.max = String(bytes.byteLength / 8 - 1); position.disabled = false; play.disabled = false;
            get('[data-info]').textContent = `${loadedRate} S/s · ${count.value} samples · ${(Number(count.value) / loadedRate).toFixed(3)} s`;
            draw();
          }
          get('[data-status]').textContent = l('Ready', '就绪');
        } catch (error) {
          if (!disposed) get('[data-status]').textContent = String(error instanceof Error ? error.message : error);
        } finally {
          busy = false;
          if (!disposed) body.querySelectorAll<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>('form input,form button,form select').forEach(el => { el.disabled = false; });
        }
      };
      body.querySelector('form')!.addEventListener('submit', event => { event.preventDefault(); void run(false); });
      get('[data-download]').addEventListener('click', () => { void run(true); });
      get('[data-download-full]').addEventListener('click', () => { if (!busy) streamDownload(true); });
      get('[data-download-collection]').addEventListener('click', () => { if (!busy) { pause(); nativeDownload('collection', `${task.task_id}-collection.sigmf`); } });
      segmentSelect.addEventListener('change', defaults);
      for (const input of [start, count]) input.addEventListener('input', clear);
      position.addEventListener('input', () => { pause(); draw(); });
      play.addEventListener('click', () => {
        if (playing) { pause(); return; }
        if (!samples) return;
        if (position.value === position.max) position.value = '0';
        playing = true; labels(); let previous = performance.now();
        timer = window.setInterval(() => {
          if (surface.root.hidden || surface.body.hidden || body.ownerDocument.hidden) { pause(); return; }
          const now = performance.now(), next = Number(position.value) + Math.round((now - previous) * loadedRate / 1000); previous = now;
          position.value = String(Math.min(Number(position.max), next)); draw();
          if (next >= Number(position.max)) pause();
        }, 33);
      });
      defaults(); labels();
      const disposeDemod = mountRecordingDemod(body, { task, modes: options.demodulateModes?.() ?? [], frequencyOffset: options.demodulateOffset?.() ?? false, selection: () => {
        const segment = segments[Number(segmentSelect.value)]; if (!segment) throw new Error(l('No saved samples', '没有已保存样本'));
        const query = recordingRange(segment.samples, start.value, count.value); query.set('segment', segment.segment_id!);
        return { query, segment };
      } });
      const unsubscribe = onLangChange(labels);
      let disposeDelete: (() => void) | undefined;
      current = { task: task.task_id, window: surface, dispose: () => { disposed = true; disposeDelete?.(); abort?.abort(); pause(); samples = null; disposeDemod(); unsubscribe(); surface.dispose(); } };
      surface.show();
      if (options.session) disposeDelete = mountRecordingDelete(body, options.session, task, () => { current?.dispose(); current = null; });
    },
    recall: (reset = false) => current?.window.recall(reset),
    dispose: () => { current?.dispose(); current = null; },
  };
}
