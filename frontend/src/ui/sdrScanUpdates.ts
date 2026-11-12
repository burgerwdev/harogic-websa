// Notifications carry no snapshots. Coalesce bursts into the existing bounded
// refresh; retain polling while disconnected and close hidden-window streams.
export class ScanUpdates {
  private source: EventSource | null = null;
  constructor(private readonly refresh: () => void) {}
  setVisible(visible: boolean): void {
    if (!visible) { this.source?.close(); this.source = null; return; }
    if (this.source || typeof EventSource === 'undefined') return;
    const source = this.source = new EventSource('/api/sdr/scan/events');
    source.onopen = source.onmessage = () => { if (this.source === source) this.refresh(); };
  }
  poll(retry = false): void { if (retry || !this.source || this.source.readyState !== EventSource.OPEN) this.refresh(); }
  close(): void { this.setVisible(false); }
}
