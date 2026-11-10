import type { ReceiverState } from '../core/multichannel';
import { getLang } from '../core/i18n';
import { copyDetailsButton } from './copyDetails';

const formatted = new WeakMap<ReceiverState, { reception?: string; processing?: string }>();
const esc = (value: string): string => value.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
/** Format the requested snapshot once, independently of live receiver refreshes. */
export function receiverDiagnosticDetails(detail?: ReceiverState, processing = false): string {
  const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
  let text = '';
  if (detail) {
    let cached = formatted.get(detail);
    if (!cached) { cached = {}; formatted.set(detail, cached); }
    const key = processing ? 'processing' : 'reception';
    text = cached[key] ??= esc(JSON.stringify(processing
      ? { runtime: detail.processing_runtime, statistics: detail.processing_statistics }
      : { stream: detail.stream, clock: detail.clock, reception: detail.reception, processing: detail.processing_runtime,
        statistics: detail.processing_statistics, fault: detail.fault, processing_fault: detail.processing_fault }, null, 2));
  }
  return `<p>${l('Diagnostic snapshot · refresh manually', '诊断快照 · 手动刷新')}</p><button class="btn" type="button" data-action="refresh-diagnostics">${l('Refresh', '刷新')}</button>${copyDetailsButton()}<pre style="white-space:pre-wrap;overflow-wrap:anywhere">${text}</pre>`;
}

/** One request on expansion or explicit refresh; live updates never poll diagnostics. */
export class ReceiverDiagnostics {
  value?: ReceiverState;
  private key = '';
  private uid?: string;
  private abort?: AbortController;
  constructor(private readonly load: (uid: string, signal: AbortSignal) => Promise<ReceiverState | undefined>,
    private readonly changed: () => void) {}
  update(row: ReceiverState | undefined, scope: string, active: boolean): void {
    const key = row && active ? `${scope}:${row.receiver_uid}` : '';
    if (this.key === key) return;
    this.dispose(); this.key = key; this.uid = row?.receiver_uid;
    if (key) this.refresh();
  }
  refresh(): void {
    if (!this.key || !this.uid) return;
    this.abort?.abort();
    const abort = this.abort = new AbortController(), key = this.key;
    void this.load(this.uid, abort.signal).then(value => {
      if (abort.signal.aborted || this.key !== key) return;
      this.value = value; this.changed();
    }).catch(() => { /* A failed diagnostic request must not disturb reception. */ });
  }
  dispose(): void {
    this.abort?.abort(); this.abort = undefined;
    this.key = ''; this.uid = undefined; this.value = undefined;
  }
}
