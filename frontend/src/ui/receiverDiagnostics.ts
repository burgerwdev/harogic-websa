import type { ReceiverState } from '../core/multichannel';

/** A view owns diagnostic demand. Live snapshots never carry detailed histories. */
export class ReceiverDiagnostics {
  value?: ReceiverState;
  private key = '';
  private abort?: AbortController;
  private timer?: ReturnType<typeof setTimeout>;
  constructor(private readonly load: (uid: string, signal: AbortSignal) => Promise<ReceiverState | undefined>,
    private readonly changed: () => void) {}
  update(row: ReceiverState | undefined, scope: string, active: boolean): void {
    const key = row && active ? `${scope}:${row.receiver_uid}:${row.revision}:${row.processing_runtime?.processing_generation}:${row.stream?.epoch}:${row.stream?.channel_id}` : '';
    if (this.key === key) return;
    this.dispose(); this.key = key;
    if (!key || !row) return;
    const abort = this.abort = new AbortController();
    const refresh = async (): Promise<void> => {
      try {
        const value = await this.load(row.receiver_uid, abort.signal);
        if (abort.signal.aborted || this.key !== key) return;
        this.value = value; this.changed();
      } catch { /* A failed diagnostic request must not disturb reception. */ }
      if (!abort.signal.aborted && this.key === key) this.timer = setTimeout(() => void refresh(), 2000);
    };
    void refresh();
  }
  dispose(): void {
    this.abort?.abort(); this.abort = undefined;
    clearTimeout(this.timer); this.timer = undefined;
    this.key = ''; this.value = undefined;
  }
}
