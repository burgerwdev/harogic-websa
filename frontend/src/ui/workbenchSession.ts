import { clone, receiverHasAudio, type ReceiverAdapter, type ServiceSnapshot, type ReceiverConfig, type Fault,
  type MonitorRoute, type ViewBinding, type ServiceCommand, type CommandReply } from '../core/multichannel';
import { ReceiverModeMemory } from './receiverModeMemory';

export class WorkbenchSession {
  snapshot: ServiceSnapshot;
  selected: string;
  monitorMode: 'manual' | 'follow' = 'manual';
  masterGain = 0.7;
  masterMuted = false;
  routes = new Map<string, MonitorRoute>();
  edits = new Map<string, { desired: ReceiverConfig; attempted: ReceiverConfig; pending: boolean; request_id: string | null; fault: Fault | null }>();
  fault: Fault | null = null;
  readonly modeMemory = new ReceiverModeMemory();
  private listeners = new Set<() => void>();
  private resultListeners = new Set<(uid: string) => void>();
  private sequence = 0;
  private stop: () => void;
  constructor(readonly adapter: ReceiverAdapter) {
    this.snapshot = adapter.snapshot(); this.selected = this.snapshot.receivers[0]?.receiver_uid ?? '';
    this.modeMemory.observe(this.snapshot);
    this.stop = adapter.subscribe(snapshot => this.accept(snapshot));
  }
  subscribe(fn: () => void): () => void { this.listeners.add(fn); return () => this.listeners.delete(fn); }
  notify(): void { this.listeners.forEach(fn => fn()); }
  subscribeResults(fn: (uid: string) => void): () => void { this.resultListeners.add(fn); return () => this.resultListeners.delete(fn); }
  notifyResults(uid: string): void { this.resultListeners.forEach(fn => fn(uid)); }
  private accept(snapshot: ServiceSnapshot): void {
    if (snapshot.service_id === this.snapshot.service_id && BigInt(snapshot.revision) < BigInt(this.snapshot.revision)) return;
    this.snapshot = snapshot;
    this.modeMemory.observe(snapshot);
    const active = new Set(snapshot.receivers.map(r => r.receiver_uid));
    for (const uid of this.routes.keys()) if (!active.has(uid)) this.routes.delete(uid);
    for (const [uid, edit] of this.edits) if (!active.has(uid) && !edit.pending) this.edits.delete(uid);
    if (!snapshot.receivers.some(r => r.receiver_uid === this.selected)) this.selected = snapshot.receivers[0]?.receiver_uid ?? '';
    this.notify();
  }
  requestId(): string { return `workbench-${++this.sequence}`; }
  resolve(binding: ViewBinding): string { return binding.type === 'follow' ? this.selected : binding.receiver_uid; }
  select(uid: string): void { this.selected = uid; this.notify(); }
  route(uid: string): MonitorRoute { return this.routes.get(uid) ?? { enabled: false, gain: 0.7, pan: 0, solo: false }; }
  listening(uid: string): boolean { return this.monitorMode === 'follow' ? uid === this.selected : this.route(uid).enabled; }
  monitor(uid: string, patch: Partial<MonitorRoute>): void {
    if (this.monitorMode === 'follow' && (patch.enabled !== undefined || patch.solo !== undefined)) {
      // A direct listening action keeps the editing target and becomes a manual
      // mix, initially preserving the source the user was already listening to.
      if (this.selected) this.routes.set(this.selected, { ...this.route(this.selected), enabled: true });
      this.monitorMode = 'manual';
    }
    this.routes.set(uid, { ...this.route(uid), ...patch }); this.notify();
  }
  private monitorEligible(uid: string): boolean {
    const r = this.snapshot.receivers.find(r => r.receiver_uid === uid);
    if (!r || r.fault || r.state !== 'running' || !receiverHasAudio(this.snapshot, r.confirmed)) return false;
    const route = this.route(uid);
    if (this.monitorMode === 'follow') return uid === this.selected && this.masterGain > 0 && route.gain > 0;
    const solo = this.snapshot.receivers.some(r => r.state === 'running' && receiverHasAudio(this.snapshot, r.confirmed) && this.route(r.receiver_uid).enabled && this.route(r.receiver_uid).solo);
    return route.enabled && (!solo || route.solo) && route.gain > 0 && this.masterGain > 0;
  }
  /** A scratch receiver can revisit a saved frequency without doubling the same audio. */
  duplicateMonitor(uid: string): string | null {
    if (!this.monitorEligible(uid)) return null;
    const receiver = this.snapshot.receivers.find(r => r.receiver_uid === uid)!;
    const signature = (r: typeof receiver): string | null => r.audio_signature
      ?? (this.snapshot.transport === 'fixture' ? JSON.stringify({ frequency: r.confirmed.baseband.frequency_hz,
        processors: r.confirmed.processors.map(({ id: _id, ...settings }) => settings) }) : null);
    const key = signature(receiver);
    if (key === null) return null;
    const route = this.route(uid);
    return this.snapshot.receivers.filter(r => r.display_number < receiver.display_number && this.monitorEligible(r.receiver_uid)
      && signature(r) === key && this.route(r.receiver_uid).gain === route.gain && this.route(r.receiver_uid).pan === route.pan)
      .sort((a, b) => a.display_number - b.display_number)[0]?.receiver_uid ?? null;
  }
  audible(uid: string): boolean { return this.monitorEligible(uid) && this.duplicateMonitor(uid) === null; }
  async send(command: ServiceCommand): Promise<CommandReply> {
    let reply: CommandReply;
    try { reply = await this.adapter.dispatch(command); }
    catch (error) { reply = { request_id: command.request_id, snapshot: this.snapshot,
      fault: { scope: 'request', code: 'TRANSPORT', message: String(error), retryable: true } }; }
    this.fault = reply.fault; this.accept(reply.snapshot); return reply;
  }
  /** One in-flight request per receiver, latest queued candidate. Never retargets on selection. */
  async configure(uid: string, config: ReceiverConfig): Promise<void> {
    const existing = this.edits.get(uid);
    if (existing?.pending) { existing.desired = clone(config); this.notify(); return; }
    const edit = { desired: clone(config), attempted: clone(config), pending: true, request_id: null as string | null, fault: null as Fault | null };
    this.edits.set(uid, edit);
    while (true) {
      const r = this.snapshot.receivers.find(r => r.receiver_uid === uid); if (!r) break;
      const sent = clone(edit.desired), request_id = this.requestId(); edit.request_id = request_id; edit.attempted = clone(sent); this.notify();
      const reply = await this.send({ type: 'configure', request_id, receiver_uid: uid, expected_revision: r.revision, config: sent });
      edit.fault = reply.fault;
      const confirmed = this.snapshot.receivers.find(r => r.receiver_uid === uid);
      if (!confirmed) {
        edit.fault ??= { scope: 'request', code: 'NOT_FOUND', message: 'Receiver retired during configuration', receiver_uid: uid, retryable: false };
        break;
      }
      if (JSON.stringify(sent) === JSON.stringify(edit.desired)) {
        edit.desired = clone(confirmed.confirmed); break;
      }
    }
    edit.pending = false; edit.request_id = null; this.notify();
  }
  dispose(): void { this.stop(); this.listeners.clear(); this.resultListeners.clear(); }
}
