import type { AudioRecordingFormat, RecordingTask, ServiceSnapshot } from '../core/multichannel';
import { getLang } from '../core/i18n';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
export const recordingEscape = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const esc = recordingEscape;
export const formatLabel = (format: AudioRecordingFormat): string => format === 'opus' ? 'Opus' : format.toUpperCase();
export function formatAdvice(format: AudioRecordingFormat): string {
  return format === 'wav' ? l('Original samples · best for editing and analysis; larger files', '原始音频 · 适合编辑、分析；文件较大')
    : format === 'flac' ? l('Lossless 24-bit PCM · smaller files for high-quality archiving', '24 位 PCM 无损压缩 · 适合高保真存档；体积更小')
      : l('Lossy compression · smallest files for long recordings and sharing', '有损压缩 · 适合长时间录音、分享；体积最小');
}
export function formatAvailable(snapshot: ServiceSnapshot, format: AudioRecordingFormat): boolean {
  return snapshot.transport === 'fixture' || format === 'wav' || !!snapshot.capabilities[format === 'flac' ? 'audio_flac_export' : 'audio_opus_export'];
}
export function savedAudioFormat(): AudioRecordingFormat {
  try { const value = localStorage.getItem('websa-audio-recording-format'); if (value === 'wav' || value === 'flac' || value === 'opus') return value; } catch { /* session only */ }
  return 'opus';
}
export function rememberAudioFormat(format: AudioRecordingFormat): void {
  try { localStorage.setItem('websa-audio-recording-format', format); } catch { /* session only */ }
}
export function resetAudioFormat(): void {
  try { localStorage.removeItem('websa-audio-recording-format'); } catch { /* session only */ }
  document.dispatchEvent(new Event('websa:recording-default-reset'));
}
export function directAudioRecording(task: RecordingTask): boolean {
  return task.tracks.some(track => track.kind === 'audio' && track.container === 'websa-audio-files-v1');
}
export function defaultAudioFormatSelect(snapshot: ServiceSnapshot, selected: AudioRecordingFormat): string {
  const label = l('Default audio recording format', '默认音频录制格式');
  return `<label class="mc-recording-default" data-key="recording-default" title="${label} · ${l('Used for new recordings', '用于新录制')}"><span>${l('Record as', '录制格式')}</span><select data-default-audio-format aria-label="${label}">${(['opus', 'flac', 'wav'] as const).map(format => `<option value="${format}" ${selected === format ? 'selected' : ''} ${formatAvailable(snapshot, format) ? '' : 'disabled'}>${formatLabel(format)}</option>`).join('')}</select></label>`;
}
export function audioFormatMenu(snapshot: ServiceSnapshot, selected: AudioRecordingFormat, context: { receiver?: string; task?: string }): string {
  const owner = context.task ? `data-format-task="${esc(context.task)}"` : `data-format-receiver="${esc(context.receiver)}"`;
  const advice = (['wav', 'flac', 'opus'] as const).map(f => `${formatLabel(f)}: ${formatAdvice(f)}`).join('\n');
  return `<details class="mc-format-menu" data-key="format-${esc(context.task ?? context.receiver)}" data-format-menu><summary title="${esc(advice)}" aria-label="${l('Choose audio format', '选择音频格式')}: ${formatLabel(selected)}"><span>${formatLabel(selected)}</span><span aria-hidden="true">⌄</span></summary>
    <div class="mc-format-options" popover="auto" role="group" aria-label="${l('Audio format', '音频格式')}"><p>${l('Choose a format', '选择音频格式')}</p>${(['wav', 'flac', 'opus'] as const).map(format => {
      const enabled = formatAvailable(snapshot, format);
      const tip = enabled ? formatAdvice(format) : format === 'opus'
        ? snapshot.capabilities.audio_opus_export === undefined
          ? l('Update and restart the server to enable Opus', '更新并重启服务器后可使用 Opus')
          : l('Install FFmpeg with libopus on the server to enable Opus', '服务器安装含 libopus 的 FFmpeg 后可用')
        : l('This server does not support FLAC export', '此服务器尚不支持 FLAC 导出');
      return `<button type="button" data-action="choose-audio-format" data-key="choose-format-${format}" data-format="${format}" ${owner} aria-pressed="${format === selected}" title="${esc(tip)}" ${enabled ? '' : 'disabled'}><span><b>${formatLabel(format)}</b><small>${esc(enabled ? formatAdvice(format) : tip)}</small></span><span aria-hidden="true">${format === selected ? '✓' : ''}</span></button>`;
    }).join('')}</div></details>`;
}
/** Native popovers escape scrolling panels and dismiss on outside click/Escape. */
export function syncAudioFormatMenus(root: HTMLElement): void {
  root.querySelectorAll<HTMLDetailsElement>('[data-format-menu][open]').forEach(menu => {
    const popup = menu.querySelector<HTMLElement>('[popover]');
    if (!popup || typeof popup.showPopover !== 'function' || !popup.matches(':popover-open')) return;
    const view = root.ownerDocument.defaultView!;
    const scale = Number(root.closest<HTMLElement>('.mc-window')?.style.zoom) || 1;
    popup.style.width = `${Math.min(280, (view.innerWidth - 24) / scale)}px`;
    popup.style.maxHeight = `${(view.innerHeight - 24) / scale}px`;
    const anchor = menu.querySelector('summary')!.getBoundingClientRect(), box = popup.getBoundingClientRect();
    const left = Math.max(12, Math.min(anchor.right - box.width, view.innerWidth - box.width - 12));
    const below = anchor.bottom + 5;
    const top = below + box.height <= view.innerHeight - 12 ? below : Math.max(12, anchor.top - box.height - 5);
    popup.style.left = `${left / scale}px`; popup.style.top = `${top / scale}px`;
  });
}
export function wireAudioFormatMenus(root: HTMLElement): () => void {
  const toggle = (event: Event): void => {
    const target = event.target as HTMLElement;
    if (target.matches('[data-format-menu]')) {
      const menu = target as HTMLDetailsElement, popup = menu.querySelector<HTMLElement>('[popover]');
      if (!popup) return;
      if (typeof popup.showPopover !== 'function') { popup.removeAttribute('popover'); return; }
      if (menu.open && menu.isConnected) { popup.showPopover(); syncAudioFormatMenus(root); }
      else popup.hidePopover();
    } else if (target.matches('.mc-format-options[popover]') && (event as ToggleEvent).newState === 'closed') {
      const menu = target.closest<HTMLDetailsElement>('[data-format-menu]'); if (menu) menu.open = false;
    }
  };
  root.addEventListener('toggle', toggle, true);
  const close = (): void => { root.querySelectorAll<HTMLDetailsElement>('[data-format-menu][open]').forEach(menu => {
    menu.querySelector<HTMLElement>('[popover]')?.hidePopover?.(); menu.open = false;
  }); };
  const scroll = (event: Event): void => { if (!(event.target as HTMLElement).closest('.mc-format-options')) close(); };
  root.addEventListener('scroll', scroll, true);
  const surface = root.closest<HTMLElement>('.mc-window');
  const observer = new MutationObserver(() => { if (root.hidden || surface?.hidden) close(); });
  observer.observe(root, { attributes: true, attributeFilter: ['hidden'] });
  if (surface) observer.observe(surface, { attributes: true, attributeFilter: ['hidden'] });
  return () => { close(); observer.disconnect(); root.removeEventListener('toggle', toggle, true); root.removeEventListener('scroll', scroll, true); };
}
export function audioDownload(task: RecordingTask, format: AudioRecordingFormat, snapshot: ServiceSnapshot): string {
  const direct = directAudioRecording(task);
  if (direct) format = task.audio_format ?? 'opus';
  const path = `/api/sdr/recordings/${encodeURIComponent(task.task_id)}/audio${format === 'wav' ? '' : `/${format}`}`;
  return `<span class="mc-audio-export"><a data-key="audio-download" class="mc-audio-download${format === 'flac' ? ' mc-audio-flac' : ''}" href="${path}" download title="${esc(formatAdvice(format) + l('. Download includes audio segments and source metadata.', '。下载包含音频片段与来源信息。'))}">${l('Download', '下载')} ${formatLabel(format)}</a>${direct ? '' : audioFormatMenu(snapshot, format, { task: task.task_id })}</span>`;
}
