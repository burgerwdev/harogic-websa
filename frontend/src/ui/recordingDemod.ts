import { getLang, onLangChange } from '../core/i18n';
import type { RecordingTask, RecordingTrack } from '../core/multichannel';
import { downloadResult } from './receiverExport';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const MAX_TAR = 20 * 1024 * 1024;

/** Accept exactly the bounded server render envelope; never unpack paths into storage. */
export function demodWav(bytes: ArrayBuffer): Uint8Array {
  if (bytes.byteLength > MAX_TAR) throw new Error('File render exceeds 20 MiB');
  const data = new Uint8Array(bytes), decoder = new TextDecoder();
  let wav: Uint8Array | null = null, offset = 0;
  const names = new Set<string>();
  while (offset + 512 <= data.length) {
    const head = data.subarray(offset, offset + 512);
    if (head.every(byte => byte === 0)) break;
    const name = decoder.decode(head.subarray(0, 100)).replace(/\0.*$/s, '');
    const sizeText = decoder.decode(head.subarray(124, 136)).replace(/\0.*$/s, '').trim();
    if (!/^[0-7]+$/.test(sizeText) || !['render.json', 'source.sigmf-meta', 'audio.wav'].includes(name) || names.has(name)) throw new Error('Invalid file render archive');
    const size = parseInt(sizeText, 8), end = offset + 512 + size;
    if (end > data.length) throw new Error('Truncated file render');
    names.add(name);
    if (name === 'audio.wav') wav = data.subarray(offset + 512, end);
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  if (!wav || names.size !== 3 || data.length - offset < 1024 || data.subarray(offset).some(byte => byte !== 0)
    || wav.length < 56 || decoder.decode(wav.subarray(0, 4)) !== 'RIFF' || decoder.decode(wav.subarray(8, 12)) !== 'WAVE') throw new Error('Incomplete file render');
  const format = new DataView(wav.buffer, wav.byteOffset, wav.byteLength);
  if (format.getUint16(20, true) !== 3 || format.getUint16(22, true) !== 1 || format.getUint32(24, true) !== 48000
    || format.getUint16(34, true) !== 32 || format.getUint32(4, true) + 8 !== wav.length || format.getUint32(52, true) + 56 !== wav.length) throw new Error('Unexpected file audio format');
  return wav;
}

export function mountRecordingDemod(body: HTMLElement, options: {
  task: RecordingTask; modes: string[]; frequencyOffset?: boolean;
  selection(): { query: URLSearchParams; segment: RecordingTrack['segments'][number] };
}): () => void {
  if (!options.modes.length) return () => {};
  const root = body.ownerDocument.createElement('details'); root.dataset.fileDemod = '';
  root.innerHTML = `<summary></summary><div class="mc-fields"><label><span data-demod-mode-label></span><select data-demod-mode></select></label>
    <label><span data-demod-bw-label></span><input data-demod-bw type="number" min="50" max="20000" value="1500" step="any"></label>
    <label data-demod-offset-field hidden><span data-demod-offset-label></span><input data-demod-offset type="number" value="0" step="any"></label>
    <label data-demod-frequency-field hidden><span data-demod-frequency-label></span><output data-demod-frequency></output></label></div>
    <div class="mc-toolbar"><button type="button" data-demod-run></button><button type="button" data-demod-cancel hidden></button><button type="button" data-demod-save disabled></button></div>
    <p data-demod-status role="status"></p><audio data-demod-audio controls preload="metadata" style="width:100%"></audio>`;
  body.append(root);
  const get = <T extends HTMLElement>(selector: string): T => root.querySelector<T>(selector)!;
  const mode = get<HTMLSelectElement>('[data-demod-mode]'), bandwidth = get<HTMLInputElement>('[data-demod-bw]');
  const offset = get<HTMLInputElement>('[data-demod-offset]');
  get('[data-demod-offset-field]').hidden = !options.frequencyOffset;
  get('[data-demod-frequency-field]').hidden = !options.frequencyOffset;
  const run = get<HTMLButtonElement>('[data-demod-run]'), save = get<HTMLButtonElement>('[data-demod-save]'), cancel = get<HTMLButtonElement>('[data-demod-cancel]');
  const audio = get<HTMLAudioElement>('audio'), status = get('[data-demod-status]');
  for (const value of options.modes) { const option = body.ownerDocument.createElement('option'); option.value = value; option.textContent = value.toUpperCase(); mode.append(option); }
  let abort: AbortController | null = null, url: string | null = null, archive: Blob | null = null, disposed = false;
  const frequency = (): void => {
    let value = NaN;
    try { if (offset.value.trim()) value = (options.selection().segment.frequency_hz ?? NaN) + Number(offset.value); } catch { /* Invalid range has no selected source. */ }
    get('[data-demod-frequency]').textContent = Number.isFinite(value) && value >= 0 ? value.toLocaleString('en-US', { maximumFractionDigits: 6 }) : '—';
  };
  const labels = (): void => {
    get('summary').textContent = l('Demodulate file', '文件重新解调');
    root.title = l('Selected IQ range · up to 60 s · fresh native filters · 48 kHz mono · no AGC/NR/squelch', '所选 IQ 范围 · 最多 60 秒 · 原生滤波器从零开始 · 48 kHz 单声道 · 无 AGC/降噪/静噪');
    get('[data-demod-mode-label]').textContent = l('File mode', '文件解调模式');
    get('[data-demod-bw-label]').textContent = l('Filter / Hz', '滤波带宽 / Hz');
    get('[data-demod-offset-label]').textContent = l('Offset / Hz', '频偏 / Hz');
    get('[data-demod-frequency-label]').textContent = l('Frequency / Hz', '频率 / Hz');
    offset.title = l('Relative to the recorded IQ centre. Positive selects higher frequency; the full filter must fit the recorded band.', '相对已录 IQ 中心；正值选择更高频率，完整滤波通带须落在已录频带内');
    get('[data-demod-frequency]').title = l('Recorded centre + file offset; live tuning is independent', '原录制中心 + 文件频偏；实时调谐独立');
    frequency();
    bandwidth.title = l('AM/NFM: full width; USB/LSB: one sideband width. Must fit recorded passband.', 'AM/NFM 为完整带宽；USB/LSB 为单边带宽，须落在已录通带内');
    bandwidth.min = ['usb', 'lsb'].includes(mode.value) ? '101' : '50';
    run.textContent = l('Render audio', '生成音频'); save.textContent = l('Download audio + source', '下载音频与来源'); cancel.textContent = l('Cancel', '取消');
    run.title = l('Process the selected stored IQ range without changing live reception', '处理所选已存 IQ 范围，不改变实时接收');
    save.title = l('Save the generated Float32 WAV and exact original IQ provenance', '保存已生成的 Float32 WAV 与原始 IQ 精确来源');
    audio.setAttribute('aria-label', l('File audio playback', '文件音频试听'));
    if (archive) status.textContent = l('Ready · 48 kHz · mono', '就绪 · 48 kHz · 单声道');
    else if (abort && run.disabled) status.textContent = l('Processing file…', '正在处理文件…');
  };
  const clear = (): void => {
    abort?.abort(); abort = null;
    if (audio.hasAttribute('src')) { audio.pause(); audio.removeAttribute('src'); audio.load(); }
    if (url) URL.revokeObjectURL(url); url = null; archive = null; save.disabled = true;
    run.disabled = false; cancel.hidden = true; status.textContent = '';
  };
  const generate = async (): Promise<void> => {
    clear();
    const controller = new AbortController(); abort = controller;
    try {
      const { query, segment } = options.selection();
      if (Number(query.get('count')) / (segment.actual_rate_hz ?? 0) > 60) throw new Error(l('Select at most 60 seconds', '请选择最多 60 秒'));
      query.set('mode', mode.value); query.set('if_bw', bandwidth.value);
      if (options.frequencyOffset) {
        if (!offset.value.trim() || !Number.isFinite(Number(offset.value))) throw new Error(l('Enter a finite offset in Hz', '请输入有限的频偏值（Hz）'));
        query.set('offset_hz', offset.value);
      }
      run.disabled = true; cancel.hidden = false; status.textContent = l('Processing file…', '正在处理文件…');
      const response = await fetch(`/api/sdr/recordings/${encodeURIComponent(options.task.task_id)}/demodulate?${query}`, { signal: controller.signal });
      if (!response.ok) { const error = await response.json(); throw new Error(error.error ?? `HTTP ${response.status}`); }
      if (Number(response.headers.get('Content-Length')) > MAX_TAR) throw new Error('File render exceeds 20 MiB');
      const bytes = await response.arrayBuffer();
      if (disposed || abort !== controller || controller.signal.aborted) return;
      const wav = demodWav(bytes);
      archive = new Blob([bytes], { type: 'application/x-tar' });
      url = URL.createObjectURL(new Blob([wav as Uint8Array<ArrayBuffer>], { type: 'audio/wav' })); audio.src = url;
      save.disabled = false;
      status.textContent = l('Ready · 48 kHz · mono', '就绪 · 48 kHz · 单声道');
    } catch (error) {
      if (!disposed && abort === controller && !controller.signal.aborted) status.textContent = error instanceof Error ? error.message : String(error);
    } finally { if (!disposed && abort === controller) { abort = null; run.disabled = false; cancel.hidden = true; } }
  };
  const input = (event: Event): void => { if (!(event.target as HTMLElement).closest('audio')) { clear(); frequency(); } };
  body.addEventListener('input', input); body.addEventListener('change', input);
  cancel.addEventListener('click', clear); run.addEventListener('click', () => { void generate(); });
  mode.addEventListener('change', labels);
  save.addEventListener('click', () => { if (archive) downloadResult(body.ownerDocument, archive, `${options.task.task_id}-demod.tar`); });
  const observer = new MutationObserver(() => { if (body.hidden || body.closest<HTMLElement>('.mc-window')?.hidden) { clear(); } });
  observer.observe(body, { attributes: true, attributeFilter: ['hidden'] });
  if (body.parentElement) observer.observe(body.parentElement, { attributes: true, attributeFilter: ['hidden'] });
  const unsubscribe = onLangChange(labels); labels();
  return () => { disposed = true; clear(); observer.disconnect(); unsubscribe(); body.removeEventListener('input', input); body.removeEventListener('change', input); root.remove(); };
}
