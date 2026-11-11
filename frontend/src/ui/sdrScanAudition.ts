import { receiverHasAudio, type ReceiverConfig, type ReceiverState } from '../core/multichannel';
import type { WorkbenchSession } from './workbenchSession';
export interface ScanGuard { capture_epoch: string; signal_id: string; signal_generation: string }
export const isScanPreview = (row: ReceiverState): boolean => row.confirmed.tags.some(tag => tag.startsWith('scan-audition:'));
export function scanReceiverMatches(row: ReceiverState, config: ReceiverConfig): boolean {
  return Math.abs(row.confirmed.baseband.frequency_hz - config.baseband.frequency_hz) < .5
    && row.confirmed.processors[0]?.mode === config.processors[0]?.mode
    && (['passband_hz', 'transition_hz', 'minimum_rate_hz'] as const).every(k => Math.abs(row.confirmed.baseband[k] - config.baseband[k]) < .5);
}
/** One user-requested preview receiver, using the normal receiver admission owner. */
export class ScanAudition {
  private readonly owner = Symbol('scan-audition');
  private readonly tag = `scan-audition:${crypto.randomUUID()}`;
  private owned: { uid: string; revision: string; service: string | undefined } | null = null;
  private borrowed: { uid: string; revision: string } | null = null;
  private tail: Promise<unknown> = Promise.resolve();
  private version = 0;
  playing: { signal: string; uid: string; frequency: number; mode: string } | null = null;
  constructor(private readonly session: WorkbenchSession) {}
  private serial<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.tail.then(operation); this.tail = result.catch(() => {}); return result;
  }
  private ownedRow(): ReceiverState | undefined {
    if (this.owned?.service !== this.session.snapshot.service_id) return undefined;
    return this.session.snapshot.receivers.find(r => r.receiver_uid === this.owned?.uid && r.revision === this.owned.revision && r.confirmed.tags.includes(this.tag));
  }
  private recording(uid: string): boolean { return this.session.snapshot.recordings.some(r => r.receiver_uid === uid && ['preparing', 'waiting', 'recording', 'stopping'].includes(r.state)); }
  reusable(config: ReceiverConfig): ReceiverState | undefined { return this.session.snapshot.receivers.find(r => !isScanPreview(r) && scanReceiverMatches(r, config)); }
  canAllocate(): boolean { const row = this.ownedRow(); return !!row && !this.recording(row.receiver_uid) || this.session.snapshot.receivers.length < this.session.snapshot.capabilities.max_channels; }
  private async configure(row: ReceiverState, config: ReceiverConfig, scan?: ScanGuard): Promise<ReceiverState> {
    const result = await this.session.send({ type: 'configure', request_id: this.session.requestId(), receiver_uid: row.receiver_uid, expected_revision: row.revision, config, ...(scan ? { scan } : {}) });
    if (result.fault) throw new Error(result.fault.message);
    const fresh = this.session.snapshot.receivers.find(r => r.receiver_uid === row.receiver_uid); if (!fresh) throw new Error('Receiver expired'); return fresh;
  }
  private remember(row: ReceiverState): void { this.owned = { uid: row.receiver_uid, revision: row.revision, service: this.session.snapshot.service_id }; }
  private async releaseBorrowed(): Promise<void> {
    const borrowed = this.borrowed;
    const row = this.session.snapshot.receivers.find(r => r.receiver_uid === borrowed?.uid && r.revision === borrowed.revision);
    if (row && !this.recording(row.receiver_uid)) await this.configure(row, { ...row.confirmed, enabled: false });
    this.borrowed = null;
  }
  private async releaseOwned(): Promise<void> {
    const row = this.ownedRow();
    // User edits or recording take ownership. Never retire a receiver the user
    // changed while it was being previewed, nor any pre-existing saved channel.
    if (!row || this.recording(row.receiver_uid)) { this.owned = null; return; }
    const result = await this.session.send({ type: 'retire', request_id: this.session.requestId(), receiver_uid: row.receiver_uid });
    if (result.fault) throw new Error(result.fault.message);
    this.owned = null;
  }
  start(signal: string, config: ReceiverConfig, guard: ScanGuard, exclusive: boolean): Promise<void> {
    const version = ++this.version, service = this.session.snapshot.service_id;
    return this.serial(async () => {
      if (version !== this.version || service !== this.session.snapshot.service_id) return;
      let row = this.reusable(config);
      if (this.borrowed?.uid !== row?.receiver_uid) await this.releaseBorrowed();
      if (row) {
        await this.releaseOwned();
        if (!row.confirmed.enabled) { row = await this.configure(row, { ...row.confirmed, enabled: true }, guard); this.borrowed = { uid: row.receiver_uid, revision: row.revision }; }
      } else {
        row = this.ownedRow();
        if (row && this.recording(row.receiver_uid)) { this.owned = null; row = undefined; }
        const preview = { ...config, name: `◉ ${config.name}`, tags: [...config.tags, this.tag] };
        if (row) row = await this.configure(row, preview, guard);
        else {
          const result = await this.session.create(preview, guard); if (result.fault) throw new Error(result.fault.message);
          row = this.session.snapshot.receivers.find(r => r.receiver_uid === result.receiver_uid);
        }
        if (!row) throw new Error('Preview receiver absent'); this.remember(row);
      }
      if (version !== this.version || service !== this.session.snapshot.service_id || guard.capture_epoch !== this.session.snapshot.capture.epoch) return;
      this.playing = { signal, uid: row.receiver_uid, frequency: config.baseband.frequency_hz, mode: config.processors[0]?.mode ?? '' };
      this.session.auditionStart(this.owner, row.receiver_uid, exclusive);
    });
  }
  setExclusive(exclusive: boolean): void { if (this.playing) this.session.auditionStart(this.owner, this.playing.uid, exclusive); }
  stop(): Promise<void> {
    ++this.version; this.playing = null; this.session.auditionStop(this.owner);
    return this.serial(async () => { await this.releaseBorrowed(); await this.releaseOwned(); });
  }
  add(config: ReceiverConfig, guard: ScanGuard): Promise<ReceiverState | undefined> {
    const version = ++this.version, service = this.session.snapshot.service_id;
    return this.serial(async () => {
      let row = this.reusable(config), owned = this.ownedRow();
      if (!row && owned && scanReceiverMatches(owned, config)) {
        row = await this.configure(owned, { ...owned.confirmed, name: config.name, tags: owned.confirmed.tags.filter(tag => tag !== this.tag) }, guard);
        this.owned = null; // Promotion retains the same receiver and IQ stream.
      } else {
        await this.releaseOwned();
        if (!row) {
          const result = await this.session.create(config, guard); if (result.fault) throw new Error(result.fault.message);
          row = this.session.snapshot.receivers.find(r => r.receiver_uid === result.receiver_uid);
        }
      }
      if (row?.receiver_uid === this.borrowed?.uid) this.borrowed = null; else await this.releaseBorrowed();
      this.playing = null; this.session.auditionStop(this.owner);
      if (version !== this.version || service !== this.session.snapshot.service_id || guard.capture_epoch !== this.session.snapshot.capture.epoch) return undefined;
      if (row && receiverHasAudio(this.session.snapshot, config)) { this.session.masterMuted = false; this.session.monitor(row.receiver_uid, { enabled: true }); }
      return row;
    });
  }
  unload(): void {
    ++this.version; this.playing = null; this.session.auditionStop(this.owner);
    const borrowed = this.borrowed, saved = this.session.snapshot.receivers.find(r => r.receiver_uid === borrowed?.uid && r.revision === borrowed?.revision);
    if (saved && !this.recording(saved.receiver_uid) && this.session.snapshot.transport === 'native') {
      void fetch('/api/sdr/receivers', { method: 'POST', keepalive: true, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ type: 'configure', request_id: crypto.randomUUID(), service_id: this.session.snapshot.service_id, receiver_uid: saved.receiver_uid, expected_revision: saved.revision, config: { ...saved.confirmed, enabled: false } }) }).catch(() => {});
    }
    const row = this.ownedRow();
    if (row && !this.recording(row.receiver_uid) && this.session.snapshot.transport === 'native') {
      void fetch('/api/sdr/receivers', { method: 'POST', keepalive: true, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ type: 'retire', request_id: crypto.randomUUID(), service_id: this.session.snapshot.service_id, receiver_uid: row.receiver_uid }) }).catch(() => {});
    }
  }
}
