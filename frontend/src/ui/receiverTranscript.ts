import { decoderTextLine, type DecoderLogEntry } from '../sdr/decoderLog';
import type { CwLine } from '../sdr/cwLog';
import { cwLineFor } from './cwLine';
import { patchWorkbench } from './workbenchDom';

/** Reconstruct chronological transmissions without combining different processing streams. */
export function renderReceiverTranscript(host: HTMLElement, entries: readonly DecoderLogEntry[], utc: boolean,
  live: boolean, source?: string | null): void {
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
    current.line.marks.push(...data.marks);
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
