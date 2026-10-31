import { clone, type ReceiverAdapter, type ServiceSnapshot, type ReceiverConfig, type Fault,
  type MonitorRoute, type ViewBinding, type ServiceCommand, type CommandReply } from '../core/multichannel';

export class WorkbenchSession {
  snapshot: ServiceSnapshot;
  selected: string;
  monitorMode: 'manual' | 'follow' = 'manual';
  masterGain = 0.7;
  routes = new Map<string, MonitorRoute>();
  edits = new Map<string, { desired: ReceiverConfig; attempted: ReceiverConfig; pending: boolean; request_id: string | null; fault: Fault | null }>();
  fault: Fault | null = null;
  private listeners = new Set<() => void>();
  private sequence = 0;
  private stop: () => void;
  constructor(readonly adapter: ReceiverAdapter) {
    this.snapshot = adapter.snapshot(); this.selected = this.snapshot.receivers[0]?.receiver_uid ?? '';
    this.stop = adapter.subscribe(snapshot => this.accept(snapshot));
  }
  subscribe(fn: () => void): () => void { this.listeners.add(fn); return () => this.listeners.delete(fn); }
  notify(): void { this.listeners.forEach(fn => fn()); }
  private accept(snapshot: ServiceSnapshot): void {
    if (snapshot.service_id === this.snapshot.service_id && BigInt(snapshot.revision) < BigInt(this.snapshot.revision)) return;
    this.snapshot = snapshot;
    if (!snapshot.receivers.some(r => r.receiver_uid === this.selected)) this.selected = snapshot.receivers[0]?.receiver_uid ?? '';
    this.notify();
  }
  requestId(): string { return `workbench-${++this.sequence}`; }
  resolve(binding: ViewBinding): string { return binding.type === 'follow' ? this.selected : binding.receiver_uid; }
  select(uid: string): void { this.selected = uid; this.notify(); }
  route(uid: string): MonitorRoute { return this.routes.get(uid) ?? { enabled: false, gain: 0.7, pan: 0, solo: false }; }
  monitor(uid: string, patch: Partial<MonitorRoute>): void {
    this.routes.set(uid, { ...this.route(uid), ...patch }); this.notify();
  }
  audible(uid: string): boolean {
    const r = this.snapshot.receivers.find(r => r.receiver_uid === uid);
    if (!r || r.state !== 'running' || r.confirmed.purpose !== 'listen') return false;
    const route = this.route(uid);
    if (this.monitorMode === 'follow') return uid === this.selected && this.masterGain > 0 && route.gain > 0;
    const solo = [...this.routes.values()].some(r => r.solo);
    return route.enabled && (!solo || route.solo) && route.gain > 0 && this.masterGain > 0;
  }
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
  dispose(): void { this.stop(); this.listeners.clear(); }
}
