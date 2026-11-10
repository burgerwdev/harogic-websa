import { renderReceiverTranscript } from './receiverTranscript';
import { getLang, t } from '../core/i18n';
import { ft8GeoTexts } from './ft8GeoText';
import { ft8SessionKey, markQsoRows } from './ft8Qso';
import { clock } from '../sdr/ft8Log';
import { decoderTextLine, type DecoderImage, type DecoderLogEntry, type DecoderRaster } from '../sdr/decoderLog';
import { renderDecoderEntries } from './decoderResults';
import { downloadResult, pictureExport, pictureIsPartial, resultExport } from './receiverExport';
import { createWorkbenchPatcher } from './workbenchDom';
import './decodeResults.css';
import { setDomProperty } from './domState';
import { ft8SnrPresentation, type Ft8SnrMeasurement } from '../sdr/ft8Snr';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const esc = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const number = (value: unknown, digits = 0): string => typeof value === 'number' && Number.isFinite(value) ? value.toFixed(digits) : '—';
interface Message { key: string; time: number; snr: unknown; snrPresentation: ReturnType<typeof ft8SnrPresentation>; dt: unknown; hz: unknown; text: string; qso: string; country: string; location: string }
export interface ReceiverDecodeView {
  update(entries: readonly DecoderLogEntry[], identity: string, mode: string, evicted: number, notice: string, source?: string | null): void;
  dispose(): void;
}

/** A view owns its filters and viewing position; the session owns reception and retained data. */
export function mountReceiverDecodeView(host: HTMLElement): ReceiverDecodeView {
  const patch = createWorkbenchPatcher(host);
  const messageRows = new WeakMap<DecoderLogEntry, { language: string; rows: Message[] }>();
  let messageTable: { key: string; rows: readonly Message[]; html: string } | undefined;
  let entries: readonly DecoderLogEntry[] = [], identity = '', mode = '', notice = '', evicted = 0;
  let activeSource: string | null | undefined;
  const clearedText = new Map<number, number>();
  let utc = false, search = '', modeFilter = 'current', cleared = 0, confirmClear = false;
  let page = 0, sort: 'time' | 'snr' | 'dt' | 'hz' | 'text' | 'country' | 'location' = 'time', direction = -1;
  let imageId: number | undefined, followImage = true;
  let exportFormat: 'csv' | 'jsonl' = 'csv', exportError = '', exporting = false, disposed = false;
  let visibleEntries: DecoderLogEntry[] = [], selectedImage: DecoderLogEntry | undefined;
  let language = getLang();
  let hovered: HTMLElement | null = null, focused: HTMLElement | null = null;
  const mark = (): void => {
    if (hovered && !host.contains(hovered)) hovered = null;
    if (focused && !host.contains(focused)) focused = null;
    markQsoRows(host, hovered ?? focused);
  };
  const qsoRow = (target: EventTarget | null): HTMLElement | null => {
    const row = (target as HTMLElement | null)?.closest?.<HTMLElement>('tr.ft8-row') ?? null;
    return row && host.contains(row) ? row : null;
  };
  const pointerOver = (event: Event): void => { hovered = qsoRow(event.target); mark(); };
  const pointerOut = (event: Event): void => { hovered = qsoRow((event as PointerEvent).relatedTarget); mark(); };
  const focusIn = (event: Event): void => { focused = qsoRow(event.target); mark(); };
  const focusOut = (event: Event): void => { focused = qsoRow((event as FocusEvent).relatedTarget); mark(); };
  const draw = (): void => {
    const modes = [...new Set([mode, ...entries.map(entry => entry.mode)].filter(Boolean))];
    const shown = entries.flatMap(entry => {
      if (entry.id > cleared || entry.kind === 'status') return [entry];
      const cw = decoderTextLine(entry), offset = clearedText.get(entry.id);
      return cw && offset !== undefined && cw.text.length > offset
        ? [{ ...entry, data: { ...cw, text: cw.text.slice(offset), marks: cw.marks.slice(offset) } }] : [];
    }).filter(entry => !(entry.kind === 'status' && object(entry.data) && (entry.data.type === 'sstv-mode'
        || entry.data.type === 'nr-status' && !entry.data.reason && ['off', 'ready'].includes(String(entry.data.state))))
      && (modeFilter === 'all' || entry.mode === (modeFilter === 'current' ? mode : modeFilter)));
    const hasMessages = shown.some(entry => !['status', 'image'].includes(entry.kind));
    const searchable = hasMessages || ['ft8', 'ft4', 'wspr', 'cw', 'rtty', 'rtty-reverse'].includes(modeFilter === 'current' ? mode : modeFilter);
    const csvAvailable = shown.some(entry => entry.kind === 'table' && object(entry.data) && Array.isArray(entry.data.decodes));
    if (!csvAvailable) exportFormat = 'jsonl';
    const images = shown.filter(entry => entry.kind === 'image');
    if (followImage || !images.some(entry => entry.id === imageId)) imageId = images[0]?.id;
    const imageIndex = images.findIndex(entry => entry.id === imageId), image = images[imageIndex];
    selectedImage = image;
    visibleEntries = shown.filter(entry => !['image', 'status'].includes(entry.kind)).flatMap(entry => {
      if (!search || !searchable) return [entry];
      if (object(entry.data) && Array.isArray(entry.data.decodes)) {
        const decodes = entry.data.decodes.filter(row => object(row) && String(row.msg ?? row.message ?? row.text ?? '').toLocaleLowerCase().includes(search.toLocaleLowerCase()));
        return decodes.length ? [{ ...entry, data: { ...entry.data, decodes } }] : [];
      }
      const cw = decoderTextLine(entry);
      return (cw ? cw.text : JSON.stringify(entry.data)).toLocaleLowerCase().includes(search.toLocaleLowerCase()) ? [entry] : [];
    });
    const messages: Message[] = [];
    const generic: DecoderLogEntry[] = [], textEntries: DecoderLogEntry[] = [];
    for (const entry of shown) {
      const data = entry.data;
      if (entry.kind === 'table' && object(data) && Array.isArray(data.decodes)) {
        let cached = messageRows.get(entry);
        if (!cached || cached.language !== getLang()) {
          const rows = data.decodes.flatMap((row, index): Message[] => {
            if (!object(row)) return [];
            const text = String(row.msg ?? row.message ?? row.text ?? '');
            return [{ key: `${entry.id}:${index}`, time: typeof row.slot_utc === 'number' ? row.slot_utc * 1000 : entry.at,
              snr: row.snr_db, snrPresentation: ft8SnrPresentation(row.snr_db, object(row.measurement) ? row.measurement as unknown as Ft8SnrMeasurement : undefined),
              dt: row.dt_s, hz: row.audio_hz, text, ...(['ft8', 'ft4'].includes(entry.mode) ? ft8GeoTexts(text) : { country: '', location: '' }), qso: ['ft8', 'ft4'].includes(entry.mode) ? ft8SessionKey(text) : '' }];
          });
          cached = { language: getLang(), rows }; messageRows.set(entry, cached);
        }
        for (const row of cached.rows) if (!search || row.text.toLocaleLowerCase().includes(search.toLocaleLowerCase())) messages.push(row);
      } else if (decoderTextLine(entry)) {
        if (!search || decoderTextLine(entry)!.text.toLocaleLowerCase().includes(search.toLocaleLowerCase())) textEntries.push(entry);
      } else if (entry.kind !== 'image' && (!search || entry.kind === 'status' || JSON.stringify(data).toLocaleLowerCase().includes(search.toLocaleLowerCase()))) generic.push(entry);
    }
    const hasGeo = messages.some(row => row.country !== '');
    messages.sort((a, b) => {
      const left = a[sort], right = b[sort];
      return direction * (typeof left === 'number' && typeof right === 'number' ? left - right : String(left ?? '').localeCompare(String(right ?? '')));
    });
    const pages = Math.max(1, Math.ceil(messages.length / 50)); page = Math.min(page, pages - 1);
    const header = (key: typeof sort, text: string): string => `<th aria-sort="${sort === key ? direction === 1 ? 'ascending' : 'descending' : 'none'}"><button type="button" title="${esc(l(`Sort by ${text}`, `按${text}排序`))}" data-sort="${key}" data-key="sort-${key}">${esc(text)}${sort === key ? direction === 1 ? ' ▴' : ' ▾' : ''}</button></th>`;
    const tableKey = `${sort}:${direction}:${page}:${utc}:${getLang()}`;
    if (!messageTable || messageTable.key !== tableKey || messageTable.rows.length !== messages.length
      || messageTable.rows.some((row, i) => row !== messages[i])) {
      messageTable = { key: tableKey, rows: [...messages], html: `<section data-key="messages" class="mc-decode-messages" ${messages.length ? '' : 'hidden'}>
      <div data-key="message-pages" class="mc-message-pages"><span>${messages.length} ${l('messages', '条消息')}</span><button type="button" data-key="previous" title="${l('Previous page of messages', '查看上一页消息')}" aria-label="${l('Previous page', '上一页')}" data-decode-action="previous" ${page === 0 ? 'disabled' : ''}>‹</button><span>${page + 1} / ${pages}</span><button type="button" data-key="next" title="${l('Next page of messages', '查看下一页消息')}" aria-label="${l('Next page', '下一页')}" data-decode-action="next" ${page + 1 >= pages ? 'disabled' : ''}>›</button></div>
      <div data-key="message-scroll" class="decoder-table-scroll"><table class="mc-message-table"><thead><tr>${header('time', `${t('ft8_time')} (${utc ? 'UTC' : 'LT'})`)}${header('snr', 'SNR / dB')}${header('dt', 'Δt / s')}${header('hz', l('Audio / Hz', '音频 / Hz'))}${header('text', t('ft8_message'))}${hasGeo ? header('country', t('ft8_country')) + header('location', t('ft8_location')) : ''}</tr></thead>
      <tbody>${messages.slice(page * 50, page * 50 + 50).map(row => `<tr data-key="${row.key}" ${row.qso ? `class="ft8-row" data-qso="${esc(row.qso)}" tabindex="0"` : ''}><td>${clock(row.time, utc)}</td><td class="${row.snrPresentation.degraded ? 'ft8-snr-degraded' : ''}" title="${esc(row.snrPresentation.title)}">${esc(row.snrPresentation.text)}</td><td>${number(row.dt, 2)}</td><td>${number(row.hz)}</td><td class="decoder-message-cell">${esc(row.text)}</td>${hasGeo ? `<td class="mc-message-country">${esc(row.country || '—')}</td><td class="mc-message-location" title="${l('Transmitted grid cell centre', '消息所含网格的中心坐标')}">${esc(row.location || '—')}</td>` : ''}</tr>`).join('')}</tbody></table></div>
    </section>` };
    }
    const changed = patch(`<div data-key="tools" class="mc-decode-tools">
      <label ${modes.length < 2 ? 'hidden' : ''}>${l('Results', '结果')} <select data-result-mode aria-label="${l('Result mode', '结果模式')}"><option value="current" ${modeFilter === 'current' ? 'selected' : ''}>${l('Current mode', '当前模式')} · ${esc(mode.toUpperCase() || 'IQ')}</option><option value="all" ${modeFilter === 'all' ? 'selected' : ''}>${l('All modes', '全部模式')}</option>${modes.map(m => `<option value="${esc(m)}" ${modeFilter === m ? 'selected' : ''}>${esc(m.toUpperCase())}</option>`).join('')}</select></label>
      <input ${searchable ? '' : 'hidden'} title="${l('Filter received text; plain text, case insensitive', '按普通文本筛选已接收内容，不区分大小写')}" data-decode-search type="search" value="${esc(search)}" placeholder="${l('Filter messages', '筛选消息')}" aria-label="${l('Filter messages', '筛选消息')}">
      <button ${hasMessages || images.length ? '' : 'hidden'} type="button" data-decode-action="utc" data-key="utc" aria-pressed="${utc}" title="${utc ? l('Switch timestamps to local time', '将时间切换为本地时间') : l('Switch timestamps to UTC', '将时间切换为 UTC')}">UTC</button>
      <button ${hasMessages || images.length ? '' : 'hidden'} type="button" title="${l('Hide messages and pictures only in this view', '仅隐藏此视图已有的消息和图像')}" data-decode-action="clear" data-key="clear">${l('Clear view', '清空视图')}</button>
      <span data-key="export-tools" class="mc-result-export" ${hasMessages ? '' : 'hidden'}><select data-export-format aria-label="${l('Export format', '导出格式')}"><option value="csv" ${exportFormat === 'csv' ? 'selected' : ''} ${csvAvailable ? '' : 'hidden disabled'}>CSV</option><option value="jsonl" ${exportFormat === 'jsonl' ? 'selected' : ''}>JSONL</option></select><button type="button" data-key="export" data-decode-action="export" title="${l('Download all retained messages matching this view’s filter; includes all pages', '下载符合本视图筛选条件的已保留消息，包含全部分页')}" ${visibleEntries.length ? '' : 'disabled'}>${l('Export messages', '导出消息')}</button></span>
    </div>
    <p data-key="export-error" class="mc-error" role="alert" ${exportError ? '' : 'hidden'}>${esc(exportError)}</p>
    <div data-key="clear-confirm" class="mc-decode-confirm" ${confirmClear ? '' : 'hidden'} role="group" aria-label="${l('Clear this view', '清空此视图')}"><span>${l('Hide retained messages and pictures in this view? Reception and backend recordings continue.', '隐藏此视图已有的消息和图像？接收与后端记录继续。')}</span><button type="button" data-decode-action="clear-confirm" data-key="confirm" title="${l('Hide retained results here; other views and recording continue', '隐藏本窗口已有结果；其他视图与录制继续')}">${l('Clear', '清空')}</button><button type="button" data-decode-action="clear-cancel" data-key="cancel" title="${l('Keep the results visible', '保留当前结果')}">${l('Cancel', '取消')}</button></div>
    <p data-key="notice" class="mc-notice mc-decode-notice" role="status" ${!notice && shown.length && shown.every(entry => entry.kind === 'status') ? 'hidden' : ''}>${esc(notice || (shown.length ? shown.every(entry => entry.kind === 'status') ? l('Live signal information', '实时信号信息') : `${l('Retained', '已保留')} ${shown.filter(e => e.kind !== 'status').length} · ${evicted ? `${l('Earlier results released', '较早结果已释放')} ${evicted}` : l('Session history', '本次会话历史')}` : mode === '' ? l('IQ', 'IQ') : ['am', 'wfm', 'drm', 'drmplus'].includes(mode) ? l('Waiting for signal', '等待信号') : ['nfm', 'usb', 'lsb', 'dsb', 'pm'].includes(mode) ? l('Audio', '音频') : t('decoder_waiting')))}</p>
    <div data-key="image-controls" class="mc-image-tools" ${images.length ? '' : 'hidden'}>
      <button type="button" data-key="image-previous" data-decode-action="image-previous" ${imageIndex >= images.length - 1 ? 'disabled' : ''} title="${l('View an older received picture', '查看较早接收的图像')}" aria-label="${l('Previous picture', '上一张图像')}">‹</button>
      <select data-image-select aria-label="${l('Picture history', '图像历史')}">${images.map((entry, i) => `<option value="${entry.id}" ${entry.id === imageId ? 'selected' : ''}>${i + 1} · ${esc((entry.data as DecoderImage | DecoderRaster).label || entry.mode.toUpperCase())} · ${clock(entry.at, utc)}</option>`).join('')}</select>
      <button type="button" data-key="image-next" data-decode-action="image-next" ${imageIndex <= 0 ? 'disabled' : ''} title="${l('View a newer received picture', '查看较新接收的图像')}" aria-label="${l('Next picture', '下一张图像')}">›</button>
      <button type="button" data-key="image-live" title="${l('Follow the latest incoming picture', '自动跟随最新接收图像')}" data-decode-action="image-live" aria-pressed="${followImage}">${l('Latest', '最新图像')}</button>
      <button type="button" data-key="image-export" data-decode-action="image-export" ${exporting ? 'disabled' : ''} title="${l('Download the selected picture; unfinished pictures are labelled partial', '下载当前图像；未完成的图像会标为部分图像')}">${l('Save PNG', '保存 PNG')}</button>
    </div>
    <section data-key="image" data-result-lane="image" class="decoder-results mc-decode-image" ${image ? '' : 'hidden'}></section>
    ${messageTable.html}
    <section data-key="transcript" data-result-lane="transcript" class="mc-text-transcript" aria-label="${l('Decoded text', '解码文本')}" ${textEntries.length ? '' : 'hidden'}></section>
    <p data-key="no-matches" class="mc-notice" ${search && !messages.length && !textEntries.length && !generic.some(entry => entry.kind !== 'status') ? '' : 'hidden'}>${l('No matching messages', '没有匹配的消息')}</p>
    <section data-key="generic" data-result-lane="generic" class="decoder-results mc-decode-log"></section>`,
    element => element.hasAttribute('data-result-lane') || element === host.ownerDocument.activeElement && element.matches('input,select'));
    if (changed) mark();
    const textHost = host.querySelector<HTMLElement>('[data-result-lane=transcript]')!;
    setDomProperty(textHost, 'hidden', !textEntries.length);
    renderReceiverTranscript(textHost, textEntries, utc, !notice, activeSource);
    const imageHost = host.querySelector<HTMLElement>('[data-result-lane=image]')!;
    setDomProperty(imageHost, 'hidden', !image);
    renderDecoderEntries(imageHost, image ? [image] : [], identity, utc, 0, 24);
    const genericHost = host.querySelector<HTMLElement>('[data-result-lane=generic]')!;
    setDomProperty(genericHost, 'hidden', !generic.length);
    renderDecoderEntries(genericHost, generic, identity, utc, evicted, 24);
  };
  const click = (event: Event): void => {
    const target = (event.target as HTMLElement).closest<HTMLElement>('[data-decode-action],[data-sort]'); if (!target) return;
    const action = target.dataset.decodeAction;
    if (target.dataset.sort) {
      const key = target.dataset.sort as typeof sort; direction = sort === key ? -direction : 1; sort = key; page = 0;
    }
    if (action === 'utc') utc = !utc;
    if (action === 'export') {
      try {
        const data = resultExport(visibleEntries, identity, exportFormat);
        downloadResult(host.ownerDocument, new Blob([data], { type: exportFormat === 'csv' ? 'text/csv;charset=utf-8' : 'application/x-ndjson' }), `websa-${mode || 'results'}-${Date.now()}.${exportFormat}`);
        exportError = '';
      } catch (error) { exportError = l('Export failed: ', '导出失败：') + String(error); }
    }
    if (action === 'image-export' && selectedImage && !exporting) {
      const selected = selectedImage, owner = identity; exporting = true; exportError = '';
      const picture = selected.data as DecoderImage | DecoderRaster;
      const partial = pictureIsPartial(picture);
      void pictureExport(host.ownerDocument, selected).then(blob => {
        downloadResult(host.ownerDocument, blob, `websa-${selected.mode}-${selected.id}${partial ? '-partial' : ''}.png`);
      }).catch(error => { if (identity === owner) exportError = l('Export failed: ', '导出失败：') + String(error); }).finally(() => { exporting = false; if (!disposed) draw(); });
    }
    if (action === 'clear') confirmClear = true;
    if (action === 'clear-cancel') confirmClear = false;
    if (action === 'clear-confirm') {
      clearedText.clear();
      for (const entry of entries) { const cw = decoderTextLine(entry); if (cw && !cw.endsLine) clearedText.set(entry.id, cw.text.length); }
      cleared = Math.max(cleared, ...entries.map(entry => entry.id)); confirmClear = false; page = 0; }
    if (action === 'previous') page = Math.max(0, page - 1);
    if (action === 'next') page++;
    const images = entries.filter(entry => entry.kind === 'image' && entry.id > cleared && (modeFilter === 'all' || entry.mode === (modeFilter === 'current' ? mode : modeFilter)));
    const index = images.findIndex(entry => entry.id === imageId);
    if (action === 'image-previous' || action === 'image-next') { imageId = images[index + (action === 'image-previous' ? 1 : -1)]?.id; followImage = false; }
    if (action === 'image-live') followImage = true;
    draw();
  };
  const input = (event: Event): void => {
    if (!(event.target as HTMLElement).matches('[data-decode-search]')) return;
    search = (event.target as HTMLInputElement).value; page = 0; draw();
  };
  const change = (event: Event): void => {
    const target = event.target as HTMLSelectElement;
    if (target.matches('[data-export-format]')) exportFormat = target.value as 'csv' | 'jsonl';
    if (target.matches('[data-result-mode]')) { modeFilter = target.value; search = ''; exportError = ''; page = 0; followImage = true; }
    if (target.matches('[data-image-select]')) { imageId = Number(target.value); followImage = false; }
    draw();
  };
  host.addEventListener('pointerover', pointerOver); host.addEventListener('pointerout', pointerOut);
  host.addEventListener('focusin', focusIn); host.addEventListener('focusout', focusOut);
  host.addEventListener('click', click); host.addEventListener('input', input); host.addEventListener('change', change);
  return {
    update(next, nextIdentity, nextMode, nextEvicted, nextNotice, nextSource): void {
      if (identity === nextIdentity && mode === nextMode && evicted === nextEvicted && notice === nextNotice && activeSource === nextSource && language === getLang()
        && entries.length === next.length && entries.every((entry, index) => entry === next[index])) return;
      if (mode !== nextMode || identity !== nextIdentity) { hovered = focused = null; }
      if (mode !== nextMode) { search = ''; exportError = ''; }
      if (identity !== nextIdentity) { patch.reset(); clearedText.clear(); search = ''; exportError = ''; cleared = 0; confirmClear = false; page = 0; modeFilter = 'current'; followImage = true; }
      entries = next; activeSource = nextSource; identity = nextIdentity; mode = nextMode; evicted = nextEvicted; notice = nextNotice; language = getLang(); draw();
    },
    dispose(): void { disposed = true; hovered = focused = null; mark();
      host.removeEventListener('pointerover', pointerOver); host.removeEventListener('pointerout', pointerOut);
      host.removeEventListener('focusin', focusIn); host.removeEventListener('focusout', focusOut); host.removeEventListener('click', click); host.removeEventListener('input', input); host.removeEventListener('change', change); },
  };
}
