import { decoderTextLine, type DecoderLogEntry } from '../sdr/decoderLog';
import type { CwLine } from '../sdr/cwLog';
import { cwLineFor } from './cwLine';
import { patchWorkbench } from './workbenchDom';
import { getLang } from '../core/i18n';

const rendered = new WeakMap<HTMLElement, { entries: readonly DecoderLogEntry[]; utc: boolean;
  live: boolean; source: string | null | undefined; language: string }>();

/** Reconstruct chronological transmissions without combining different processing streams. */
export function renderReceiverTranscript(host: HTMLElement, entries: readonly DecoderLogEntry[], utc: boolean,
  live: boolean, source?: string | null): void {
  const previous = rendered.get(host), language = getLang();
  if (previous && previous.utc === utc && previous.live === live && previous.source === source && previous.language === language
    && previous.entries.length === entries.length && previous.entries.every((entry, i) => entry === entries[i])) return;
  rendered.set(host, { entries: [...entries], utc, live, source, language });
  if (!entries.length && !host.childNodes.length) return;
  const follow = host.scrollHeight - host.scrollTop - host.clientHeight < 24;
  const rows: { key: number; source: string; mode: string; line: CwLine; complete: boolean }[] = [];
  for (const entry of [...entries].sort((a, b) => a.id - b.id)) {
    const data = decoderTextLine(entry); if (!data) continue;
    let current = rows.at(-1);
    if (!current || current.complete || current.source !== entry.source || current.mode !== entry.mode) {
      if (current) current.complete = true;
      current = { key: entry.id, source: entry.source, mode: entry.mode, line: { at: entry.at, text: '', marks: [] }, complete: false };
      rows.push(current);
    }
    current.line.text += data.text;
    // A retained transmission can exceed the engine's function-argument limit.
    for (const mark of data.marks) current.line.marks.push(mark);
    current.complete = data.endsLine;
  }
  const template = host.ownerDocument.createElement('div');
  for (const row of rows) {
    const current = live && !row.complete && row === rows.at(-1) && (source === undefined || source === row.source);
    const node = cwLineFor({ ...row.line, text: row.line.text.replace(/\n$/, '') }, current, host.ownerDocument, utc);
    node.dataset.key = String(row.key); template.append(node);
  }
  patchWorkbench(host, template.innerHTML);
  if (follow) host.scrollTop = host.scrollHeight;
}
