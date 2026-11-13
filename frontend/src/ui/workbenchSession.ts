import { clone, receiverHasAudio, type ReceiverAdapter, type ServiceSnapshot, type ReceiverConfig, type Fault,
  type MonitorRoute, type ViewBinding, type ServiceCommand, type CommandReply, type ReceiverState } from '../core/multichannel';
import { ReceiverModeMemory } from './receiverModeMemory';
import { loadWorkbenchPreferences, saveWorkbenchPreferences, workbenchAudioDefaults } from './workbenchPreferences';
import { canClearInterruption } from './receiverLive';
import { representsCurrent } from './workbenchCurrent';

const CLEARED_INTERRUPTION_KEY = 'websa-cleared-interruptions';

export class WorkbenchSession {
  snapshot: ServiceSnapshot;
  selected: string;
  private audioPreferences = workbenchAudioDefaults();
  get monitorMode(): 'manual' | 'follow' { return this.audioPreferences.monitorMode; }
  set monitorMode(mode: 'manual' | 'follow') { this.audioPreferences.monitorMode = mode; this.saveAudioPreferences(); }
  get masterGain(): number { return this.audioPreferences.masterGain; }
  set masterGain(gain: number) { this.audioPreferences.masterGain = gain; this.saveAudioPreferences(); }
  private saveAudioPreferences(): void {
    if (this.snapshot?.transport === 'native') saveWorkbenchPreferences(this.audioPreferences);
  }
  masterMuted = false;
  private audition: { owner: symbol; uid: string; exclusive: boolean; muted: boolean; epoch: string } | null = null;
  /** Temporary local playback overlay; saved routes, Solo and follow mode stay intact. */
  auditionStart(owner: symbol, uid: string, exclusive: boolean): void {
    if (this.audition && this.audition.owner !== owner) this.clearAudition();
    const muted = this.audition?.owner === owner ? this.audition.muted : this.masterMuted;
    this.audition = { owner, uid: this.monitorTarget(uid), exclusive, muted, epoch: this.snapshot.capture.epoch };
    this.masterMuted = false; this.notify();
  }
  auditionStop(owner: symbol): void {
    if (this.audition?.owner !== owner) return;
    if (!this.masterMuted) this.masterMuted = this.audition.muted;
    this.audition = null; this.notify();
  }
  private clearAudition(): void { if (this.audition) this.auditionStop(this.audition.owner); }
  routes = new Map<string, MonitorRoute>();
  edits = new Map<string, { desired: ReceiverConfig; attempted: ReceiverConfig; pending: boolean; request_id: string | null; fault: Fault | null }>();
  fault: Fault | null = null;
  readonly modeMemory:ReceiverModeMemory;
  private listeners = new Set<() => void>();
  private resultListeners = new Set<(uid: string) => void>();
  private sequence = 0;
  private commandSequence = 0;
  private faultSequence = 0;
  private recoveries = new Map<string, Promise<CommandReply>>();
  private clearedInterruptions = new Map<string, string>();
  private interruptionKey(uid:string):string {
    const recovery=this.snapshot.receivers.find(r=>r.receiver_uid===uid)?.reception;
    return recovery ? `${recovery.last_interruption.at_ms}:${recovery.interruptions}` : '';
  }
  interruptionCleared(uid:string):boolean {
    const receiver=this.snapshot.receivers.find(r=>r.receiver_uid===uid);
    return !!receiver && canClearInterruption(receiver) && this.clearedInterruptions.get(uid)===this.interruptionKey(uid);
  }
  clearInterruption(uid:string):void {
    const receiver=this.snapshot.receivers.find(r=>r.receiver_uid===uid);
    if (!receiver || !canClearInterruption(receiver)) return;
    this.clearedInterruptions.set(uid,this.interruptionKey(uid));
    try { sessionStorage.setItem(CLEARED_INTERRUPTION_KEY,JSON.stringify({service:this.snapshot.service_id,entries:[...this.clearedInterruptions]})); } catch { /* Session storage is optional. */ }
    this.notify();
  }
  recovering(uid:string):boolean {return this.recoveries.has(uid);}
  private retirements = new Map<string, Promise<CommandReply>>();
  private mutationTail: Promise<void> = Promise.resolve();
  private generation = 0;
  /** Invalidates local continuations on Preset, service replacement, and disposal. */
  get lifecycleVersion(): number { return this.generation; }
  private disposed = false;
  clearingChannels = false;
  private currentReceiverSource?: () => string | null;
  private silencedCurrent: { uid: string; hz: number } | null = null;
  setCurrentReceiverSource(source: () => string | null): void { this.currentReceiverSource = source; }
  /** A hidden Current never keeps an independent speaker behind its saved card. */
  private currentRepresentative(): string | null {
    const current = this.snapshot.receivers.find(r => r.receiver_uid === this.currentReceiverSource?.());
    if (!current) return null;
    return this.snapshot.receivers.filter(r => representsCurrent(r, current))
      .sort((a, b) => a.display_number - b.display_number)[0]?.receiver_uid ?? null;
  }
  private monitorTarget(uid: string): string { return uid === this.currentReceiverSource?.() ? this.currentRepresentative() ?? uid : uid; }
  private currentSilenced(uid: string): boolean {
    const silence = this.silencedCurrent, row = this.snapshot.receivers.find(r => r.receiver_uid === uid);
    return !!silence && silence.uid === uid && !!row && Math.abs(row.confirmed.baseband.frequency_hz - silence.hz) < .5;
  }
  private retiredServices = new Set<string | undefined>();
  private stop: () => void;
  private readonly resetPreset = (): void => {
    this.clearAudition();
    this.silencedCurrent = null;
    this.generation++; this.mutationTail = Promise.resolve(); this.recoveries.clear();
    this.clearingChannels = false;
    this.retirements.clear();
    if (this.monitorMode === 'follow' && this.selected) this.routes.set(this.selected, { ...this.route(this.selected), enabled: true });
    this.audioPreferences = workbenchAudioDefaults();
    this.modeMemory.reset(); this.edits.clear(); this.notify();
  };
  constructor(readonly adapter: ReceiverAdapter, modeMemory = new ReceiverModeMemory()) {
    this.modeMemory=modeMemory;
    this.snapshot = adapter.snapshot(); this.selected = this.snapshot.receivers[0]?.receiver_uid ?? '';
    try {
      const saved=JSON.parse(sessionStorage.getItem(CLEARED_INTERRUPTION_KEY)??'null');
      if (saved?.service===this.snapshot.service_id && Array.isArray(saved?.entries)) {
        for (const row of saved.entries) if (Array.isArray(row) && row.length===2 && row.every(v=>typeof v==='string')) this.clearedInterruptions.set(row[0],row[1]);
      }
    } catch { /* Ignore unavailable storage or stale preferences. */ }
    if (this.snapshot.transport === 'native') this.audioPreferences = loadWorkbenchPreferences();
    this.modeMemory.observe(this.snapshot);
    this.stop = adapter.subscribe(snapshot => this.accept(snapshot));
    document.addEventListener('websa:preset-applied', this.resetPreset);
  }
  subscribe(fn: () => void): () => void { this.listeners.add(fn); return () => this.listeners.delete(fn); }
  notify(): void { if (!this.disposed) for (const fn of [...this.listeners]) if (this.listeners.has(fn)) fn(); }
  subscribeResults(fn: (uid: string) => void): () => void { this.resultListeners.add(fn); return () => this.resultListeners.delete(fn); }
  notifyResults(uid: string): void { if (!this.disposed) for (const fn of [...this.resultListeners]) if (this.resultListeners.has(fn)) fn(uid); }
  private accept(snapshot: ServiceSnapshot): void {
    if (this.disposed || this.retiredServices.has(snapshot.service_id)) return;
    if (snapshot.service_id === this.snapshot.service_id && BigInt(snapshot.revision) < BigInt(this.snapshot.revision)) return;
    if (snapshot.service_id === this.snapshot.service_id && snapshot.state_revision !== undefined && this.snapshot.state_revision !== undefined
      && BigInt(snapshot.state_revision) < BigInt(this.snapshot.state_revision)) return;
    const serviceChanged = snapshot.service_id !== this.snapshot.service_id;
    if (this.audition && (serviceChanged || this.audition.epoch !== snapshot.capture.epoch || !snapshot.receivers.some(r => r.receiver_uid === this.audition!.uid))) this.clearAudition();
    if (serviceChanged) {
      this.silencedCurrent = null;
      this.retiredServices.add(this.snapshot.service_id); this.generation++;
      this.clearingChannels = false;
      this.clearedInterruptions.clear();
      this.recoveries.clear(); this.routes.clear(); this.edits.clear(); this.retirements.clear(); this.mutationTail = Promise.resolve();
    }
    this.snapshot = snapshot;
    this.modeMemory.observe(snapshot);
    const active = new Set(snapshot.receivers.map(r => r.receiver_uid));
    if (serviceChanged || this.fault?.scope === 'request' && this.fault.receiver_uid && !active.has(this.fault.receiver_uid)) this.fault = null;
    for (const uid of this.routes.keys()) if (!active.has(uid)) this.routes.delete(uid);
    for (const uid of this.clearedInterruptions.keys()) if (!active.has(uid)) this.clearedInterruptions.delete(uid);
    for (const [uid, edit] of this.edits) if (!active.has(uid) && !edit.pending) this.edits.delete(uid);
    if (!snapshot.receivers.some(r => r.receiver_uid === this.selected)) this.selected = snapshot.receivers[0]?.receiver_uid ?? '';
    this.notify();
  }
  requestId(): string { return `workbench-${++this.sequence}`; }
  resolve(binding: ViewBinding): string { return binding.type === 'follow' ? this.selected : binding.receiver_uid; }
  select(uid: string): void { this.selected = uid; this.notify(); }
  route(uid: string): MonitorRoute {
    const route = this.routes.get(uid) ?? { enabled: false, gain: 0.7, pan: 0, solo: false };
    return this.audition?.uid === uid ? { ...route, enabled: true, gain: route.gain || 0.7 } : route;
  }
  listening(uid: string): boolean {
    if (this.audition?.uid === uid) return true;
    if (this.audition?.exclusive) return false;
    if (this.monitorTarget(uid) !== uid || this.currentSilenced(uid)) return false;
    return this.monitorMode === 'follow' ? uid === this.monitorTarget(this.selected) : this.route(uid).enabled;
  }
  monitor(uid: string, patch: Partial<MonitorRoute>): void {
    if (this.clearingChannels) return;
    uid = this.monitorTarget(uid);
    const currentUid = this.currentReceiverSource?.();
    if (patch.enabled !== undefined && uid === currentUid) this.silencedCurrent = null;
    else if (patch.enabled !== undefined && uid === this.currentRepresentative()) {
      const current = this.snapshot.receivers.find(r => r.receiver_uid === currentUid);
      if (current) this.silencedCurrent = patch.enabled ? null : { uid: current.receiver_uid, hz: current.confirmed.baseband.frequency_hz };
    }
    if (this.monitorMode === 'follow' && (patch.enabled !== undefined || patch.solo !== undefined)) {
      // A direct listening action keeps the editing target and becomes a manual
      // mix, initially preserving the source the user was already listening to.
      const selected = this.monitorTarget(this.selected);
      if (selected) this.routes.set(selected, { ...this.route(selected), enabled: true });
      this.monitorMode = 'manual';
    }
    this.routes.set(uid, { ...this.route(uid), ...patch }); this.notify();
  }
  private monitorAvailable(r: ReceiverState): boolean {
    return !this.clearingChannels && !this.retiring(r.receiver_uid) && this.monitorTarget(r.receiver_uid) === r.receiver_uid
      && (this.audition?.uid === r.receiver_uid || !this.currentSilenced(r.receiver_uid))
      && r.coverage?.state !== 'out_of_band' && (!r.reception || r.reception.state === 'receiving') && !r.fault
      && r.processing_state !== 'failed' && r.state === 'running' && receiverHasAudio(this.snapshot, r.confirmed);
  }
  private monitorEligible(uid: string): boolean {
    const r = this.snapshot.receivers.find(r => r.receiver_uid === uid);
    if (!r || !this.monitorAvailable(r)) return false;
    if (this.audition?.uid === uid) return true;
    if (this.audition?.exclusive) return false;
    const route = this.route(uid);
    if (this.monitorMode === 'follow') return uid === this.monitorTarget(this.selected);
    const solo = this.snapshot.receivers.some(r => this.route(r.receiver_uid).enabled && this.route(r.receiver_uid).solo && this.monitorAvailable(r));
    return route.enabled && (!solo || route.solo);
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
  /** Selected healthy routes keep rendering PCM before the local output controls. */
  playbackActive(uid: string): boolean { return this.monitorEligible(uid) && this.duplicateMonitor(uid) === null; }
  audible(uid: string): boolean { return !this.masterMuted && this.masterGain > 0 && this.route(uid).gain > 0 && this.playbackActive(uid); }
  retiring(uid: string): boolean { return this.retirements.has(uid); }
  private invalidated(command: ServiceCommand): CommandReply {
    return { request_id: command.request_id, snapshot: this.snapshot, fault: { scope: 'request',
      code: this.disposed ? 'CLOSED' : 'SOURCE_RESTARTED', message: 'Receiver session changed before command completion', retryable: !this.disposed } };
  }
  private serialize<T>(operation: () => Promise<T>): Promise<T> {
    const pending = this.mutationTail.then(operation);
    this.mutationTail = pending.then(() => undefined, () => undefined);
    return pending;
  }
  private enqueue(command: ServiceCommand): Promise<CommandReply> {
    const generation = this.generation, captured = clone(command);
    return this.serialize(async () => {
      if (this.disposed || generation !== this.generation) return this.invalidated(captured);
      if (captured.type === 'retire' && !this.snapshot.receivers.some(r => r.receiver_uid === captured.receiver_uid))
        return { request_id: captured.request_id, snapshot: this.snapshot, fault: null };
      return this.dispatch(captured);
    });
  }
  /** Observe identity at dispatch, after earlier local mutations have committed. */
  create(config: ReceiverConfig, scan?: { capture_epoch: string; signal_id: string; signal_generation: string }): Promise<{ receiver_uid: string | null; fault: Fault | null }> {
    if (this.clearingChannels) return Promise.resolve({ receiver_uid: null, fault: this.clearingFault() });
    const command: ServiceCommand = { type: 'create', request_id: this.requestId(), config: clone(config), ...(scan ? { scan } : {}) };
    const generation = this.generation;
    return this.serialize(async () => {
      const cancelled = () => ({ receiver_uid: null, fault: this.invalidated(command).fault });
      if (this.disposed || generation !== this.generation) return cancelled();
      const known = new Set(this.snapshot.receivers.map(r => r.receiver_uid)), sequence = this.commandSequence + 1;
      const reply = await this.dispatch(command);
      if (this.disposed || generation !== this.generation) return cancelled();
      if (reply.fault) return { receiver_uid: null, fault: reply.fault };
      const added = reply.snapshot.receivers.filter(r => !known.has(r.receiver_uid));
      // The protocol returns a snapshot, not a created UID. External concurrent
      // additions are ambiguous even when names and configurations happen to match.
      if (added.length === 1) return { receiver_uid: added[0].receiver_uid, fault: null };
      const fault: Fault = { scope: 'request', code: 'IDENTITY', request_id: command.request_id,
        message: 'Channel creation completed, but its identity is ambiguous; select the channel explicitly', retryable: false };
      if (sequence >= this.faultSequence) { this.faultSequence = sequence; this.fault = fault; this.notify(); }
      return { receiver_uid: null, fault };
    });
  }
  async recover(uid:string):Promise<CommandReply> {
    const existing=this.recoveries.get(uid);if(existing) return existing;
    const receiver=this.snapshot.receivers.find(row=>row.receiver_uid===uid);
    if (!receiver) return this.invalidated({type:'retire',receiver_uid:uid,request_id:this.requestId()});
    const operation=this.send({type:'configure',receiver_uid:uid,request_id:this.requestId(),expected_revision:receiver.revision,
      config:clone(receiver.confirmed),recover:true}).finally(()=>{if(this.recoveries.get(uid)===operation)this.recoveries.delete(uid);this.notify();});
    this.recoveries.set(uid,operation);this.notify();return operation;
  }
  async send(command: ServiceCommand): Promise<CommandReply> {
    if (this.disposed) return this.invalidated(command);
    if (this.clearingChannels) return { request_id: command.request_id, snapshot: this.snapshot, fault: this.clearingFault() };
    if ((command.type === 'configure' || command.type === 'record_start' || command.type === 'output' && command.enabled)
      && this.retiring(command.receiver_uid)) return { request_id: command.request_id,
      snapshot: this.snapshot, fault: { scope: 'request', code: 'BUSY', message: 'Receiver deletion pending',
        receiver_uid: command.receiver_uid, retryable: true } };
    if (command.type !== 'retire') return ['create', 'configure', 'scene'].includes(command.type) ? this.enqueue(command) : this.dispatch(command);
    // Deleting the saved representative must not resurrect hidden Current audio.
    if (command.receiver_uid === this.currentRepresentative()) {
      const current = this.snapshot.receivers.find(r => r.receiver_uid === this.currentReceiverSource?.());
      if (current) {
        this.routes.set(current.receiver_uid, { ...this.route(current.receiver_uid), enabled: false });
        this.silencedCurrent = { uid: current.receiver_uid, hz: current.confirmed.baseband.frequency_hz };
      }
    }
    const pending = this.retirements.get(command.receiver_uid);
    if (pending) return { ...await pending, request_id: command.request_id };
    // Configuration and deletion share the native commit revision. Preserve
    // ingress order across views while coalescing duplicate retirements.
    const operation = this.enqueue(command).finally(() => {
      if (this.retirements.get(command.receiver_uid) === operation) this.retirements.delete(command.receiver_uid);
      this.notify();
    });
    this.retirements.set(command.receiver_uid, operation);
    this.notify();
    return operation;
  }
  private async dispatch(command: ServiceCommand): Promise<CommandReply> {
    const generation = this.generation, sequence = ++this.commandSequence;
    let reply: CommandReply;
    try { reply = await this.adapter.dispatch(command); }
    catch (error) { reply = { request_id: command.request_id, snapshot: this.snapshot,
      fault: { scope: 'request', code: 'TRANSPORT', message: String(error), retryable: true } }; }
    if (this.disposed || generation !== this.generation) return this.invalidated(command);
    if (reply.fault && 'receiver_uid' in command && !reply.fault.receiver_uid)
      reply = { ...reply, fault: { ...reply.fault, receiver_uid: command.receiver_uid } };
    if (sequence >= this.faultSequence) { this.faultSequence = sequence; this.fault = reply.fault; }
    this.accept(reply.snapshot); return reply;
  }
  /** One in-flight request per receiver, latest queued candidate. Never retargets on selection. */
  async configure(uid: string, config: ReceiverConfig): Promise<void> {
    if (this.disposed) return;
    const generation = this.generation;
    const existing = this.edits.get(uid);
    if (existing?.pending) { existing.desired = clone(config); this.notify(); return; }
    const edit = { desired: clone(config), attempted: clone(config), pending: true, request_id: null as string | null, fault: null as Fault | null };
    this.edits.set(uid, edit);
    while (true) {
      const r = this.snapshot.receivers.find(r => r.receiver_uid === uid); if (!r) break;
      const sent = clone(edit.desired), request_id = this.requestId(); edit.request_id = request_id; edit.attempted = clone(sent); this.notify();
      const reply = await this.send({ type: 'configure', request_id, receiver_uid: uid, expected_revision: r.revision, config: sent });
      if (this.disposed || generation !== this.generation) break;
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
  private clearingFault(): Fault { return { scope: 'request', code: 'BUSY', message: 'Clearing channels', retryable: true }; }
  /** Serialize the confirmed set with edits; never target new channels or a new service. */
  async clearChannels(uids: readonly string[], currentUid: string | null): Promise<void> {
    if (this.clearingChannels || this.disposed) return;
    const generation = this.generation;
    this.clearingChannels = true; this.monitorMode = 'manual';
    for (const uid of uids) this.routes.set(uid, { ...this.route(uid), enabled: false, solo: false });
    this.notify();
    const faults: Fault[] = [];
    try {
      await this.serialize(async () => {
        for (const uid of uids) {
          if (this.disposed || generation !== this.generation) return;
          const row = this.snapshot.receivers.find(r => r.receiver_uid === uid); if (!row) continue;
          const reply = await this.dispatch(uid === currentUid
            ? { type: 'configure', request_id: this.requestId(), receiver_uid: uid, expected_revision: row.revision, config: { ...clone(row.confirmed), enabled: false } }
            : { type: 'retire', request_id: this.requestId(), receiver_uid: uid });
          if (reply.fault) faults.push(reply.fault);
        }
      });
    } finally {
      // An invalidated clear no longer owns the busy flag or the new session's
      // errors. Its transport reply can arrive after another clear has started.
      if (!this.disposed && generation === this.generation) {
        this.clearingChannels = false;
        if (faults.length) this.fault = { ...faults[0], message: faults.map(f => f.message).join('; ') };
        this.notify();
      }
    }
  }
  dispose(): void { this.disposed = true; this.clearAudition(); this.generation++; document.removeEventListener('websa:preset-applied', this.resetPreset); this.stop(); this.listeners.clear(); this.resultListeners.clear(); this.retirements.clear(); }
}
