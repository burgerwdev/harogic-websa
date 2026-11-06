import { clone, type ReceiverAdapter, type ReceiverConfig, type ServiceSnapshot, type ServiceCommand,
  type ConfigureCommand, type SceneCommand, type Impact, type Fault, type CommandReply,
  type RecordingTask, type TaskState, type ReceiverState, type TrackKind } from '../core/multichannel';
import capacity from '../../../contracts/service-capacity.json';

// Demo seed count is independent of native capacity, which comes from the service.
const DEMO_PURPOSES = ['listen', 'digital', 'iq'] as const;
const DEMO_COLORS = ['#72dd91', '#e8cd72', '#76c9ed'];

export function fixtureConfig(purpose: ReceiverConfig['purpose'], number = 1): ReceiverConfig {
  return { name: ['本地守听', '台站 A', '未知信号'][number - 1] ?? `Channel ${number}`,
    color: DEMO_COLORS[(number - 1) % DEMO_COLORS.length], tags: [], enabled: true, purpose,
    baseband: { frequency_hz: [7_100_000, 7_074_000, 7_150_000][number - 1] ?? 7_120_000,
      passband_hz: purpose === 'listen' ? 12_500 : purpose === 'digital' ? 6_400 : 3_000, transition_hz: 1_000, minimum_rate_hz: purpose === 'digital' ? 8_000 : 6_000 },
    processors: purpose === 'iq' ? [] : [{ id: `processor-${number}`,
      kind: purpose === 'listen' ? 'demodulator' : 'decoder', mode: purpose === 'listen' ? 'nfm' : 'ft8',
      minimum_rate_hz: purpose === 'listen' ? 0 : 6_000 }] };
}
// Fixture planning only; rates preserve the native integer-clock shape without claiming DSP execution.
function fixtureRate(config: ReceiverConfig): number {
  const b = config.baseband;
  const needed = Math.max(b.passband_hz + 2*b.transition_hz, b.minimum_rate_hz, ...config.processors.map(p => p.minimum_rate_hz));
  if (!Number.isFinite(needed) || needed <= 0) return NaN;
  for (let d = Math.min(65536, Math.floor(976_562.5 / needed)); d >= 1; d--) {
    let remaining = d; for (const factor of [2,3,5,7]) while (remaining % factor === 0) remaining /= factor;
    if (remaining === 1) return 976_562.5 / d;
  }
  return NaN;
}
export function fixtureSnapshot(maxChannels: number = capacity.MAX_CHANNELS): ServiceSnapshot {
  return { contract: 'multichannel-draft-v1', transport: 'fixture', revision: '1',
    capabilities: { max_channels: maxChannels, max_recording_tasks: maxChannels, capacity_target: capacity.CAPACITY_TARGET, modes: ['nfm', 'am', 'usb', 'ft8'] },
    recording_resources: { active_tasks: 0, max_tasks: maxChannels, available_tasks: maxChannels },
    capture: { source_id: 'fixture-rf', center_hz: 7_100_000, usable_bandwidth_hz: 250_000,
      input_rate_hz: 976_562.5, epoch: '1', state: 'running' },
    receivers: DEMO_PURPOSES.slice(0, maxChannels).map((purpose, i) => ({
      receiver_uid: `fixture-c${i + 1}`, source_id: 'fixture-rf', display_number: i + 1, revision: '1',
      confirmed: fixtureConfig(purpose, i + 1), state: 'running', fault: null,
      stream: { source_id: 'fixture-rf', epoch: '1', channel_id: String(i + 1) },
      clock: { input_index: '9007199254740993', output_index: '0', fraction_num: '0', fraction_den: '1',
        input_rate_hz: 976_562.5, actual_rate_hz: fixtureRate(fixtureConfig(purpose, i + 1)), delay_seconds: null, utc_reliable: false },
    })), outputs: [], recordings: [] };
}

/** In-memory development server. It never connects to /api, hardware, files or speakers. */
export class FixtureReceiverAdapter implements ReceiverAdapter {
  private state: ServiceSnapshot;
  private listeners = new Set<(state: ServiceSnapshot) => void>();
  private timers = new Set<ReturnType<typeof setTimeout>>();
  private nextId = 10;
  private rejectNext = false;
  private delayNext = false;
  private disposed = false;
  constructor(maxChannels: number = capacity.MAX_CHANNELS, private latency = 180) { this.state = fixtureSnapshot(maxChannels); }
  snapshot(): ServiceSnapshot {
    const snapshot = clone(this.state), max = snapshot.capabilities.max_recording_tasks!;
    const active = snapshot.recordings.filter(t => !['completed', 'failed'].includes(t.state)).length;
    snapshot.recording_resources = { active_tasks: active, max_tasks: max, available_tasks: Math.max(0, max - active) };
    return snapshot;
  }
  subscribe(listener: (state: ServiceSnapshot) => void): () => void {
    this.listeners.add(listener); return () => this.listeners.delete(listener);
  }
  private emit(): void {
    this.state.revision = String(BigInt(this.state.revision) + 1n);
    this.listeners.forEach(listener => listener(this.snapshot()));
  }
  private later(fn: () => void): void {
    const timer = setTimeout(() => { this.timers.delete(timer); if (!this.disposed) fn(); }, this.latency);
    this.timers.add(timer);
  }
  dispose(): void { this.disposed = true; this.timers.forEach(clearTimeout); this.listeners.clear(); }
  preflight(command: ConfigureCommand | SceneCommand): Impact {
    const scene = command.type === 'scene';
    const center = scene ? command.capture_center_hz : this.state.capture.center_hz;
    const changes = scene ? command.configs : [{ receiver_uid: command.receiver_uid, config: command.config }];
    const affected = changes.filter(change => {
      const old = this.state.receivers.find(r => r.receiver_uid === change.receiver_uid)?.confirmed;
      return scene || !old || JSON.stringify(old.baseband) !== JSON.stringify(change.config.baseband)
        || old.enabled !== change.config.enabled || fixtureRate(old) !== fixtureRate(change.config);
    }).map(change => change.receiver_uid);
    const faults: Fault[] = [];
    const fail = (message: string): void => { faults.push({ scope: 'request', code: 'PREFLIGHT', message, retryable: true }); };
    if (!Number.isFinite(center)) fail('Invalid capture center');
    if (this.state.capture.state === 'failed' && changes.some(c => c.config.enabled)) fail('Capture failed; recover the source before enabling reception');
    if (scene && command.expected_revision !== this.state.revision) fail('Scene revision conflict');
    if (scene && (changes.length !== this.state.receivers.length || this.state.receivers.some(r => !changes.some(c => c.receiver_uid === r.receiver_uid)))) fail('Scene must include the full receiver set');
    if (new Set(changes.map(c => c.receiver_uid)).size !== changes.length) fail('Duplicate receiver identity');
    for (const change of changes) {
      const old = this.state.receivers.find(r => r.receiver_uid === change.receiver_uid);
      if (!old || (!scene && command.expected_revision !== old.revision)) fail('Receiver revision conflict');
      const c = change.config, b = c.baseband;
      if (![b.frequency_hz, b.passband_hz, b.transition_hz, b.minimum_rate_hz].every(Number.isFinite)
        || b.frequency_hz <= 0 || b.passband_hz <= 0 || b.transition_hz <= 0 || b.minimum_rate_hz <= 0) fail('Invalid baseband geometry');
      if (c.purpose === 'iq' && c.processors.length) fail('IQ purpose has no processors');
      if (!Number.isFinite(fixtureRate(c))) fail('Baseband demand exceeds source rate');
      if (c.processors.some(p => !this.state.capabilities.modes.includes(p.mode))) fail('Processor unavailable');
    }
    const all = this.state.receivers.map(r => changes.find(c => c.receiver_uid === r.receiver_uid)?.config ?? r.confirmed);
    if (all.filter(c => c.enabled).length > this.state.capabilities.max_channels) fail('Channel limit exceeded');
    for (const c of all.filter(c => c.enabled)) {
      if (Math.abs(c.baseband.frequency_hz - center) + c.baseband.passband_hz / 2 + c.baseband.transition_hz
          > (this.state.capture.usable_bandwidth_hz ?? 0) / 2) fail('Full passband and transition must fit shared capture');
    }
    return { accepted: !faults.length, faults, capture_change: center !== this.state.capture.center_hz,
      affected_receivers: affected, candidate_center_hz: center,
      affected_tracks: this.state.recordings.filter(t => affected.includes(t.receiver_uid) && t.state === 'recording')
        .flatMap(t => t.tracks.map(track => track.track_id)),
      estimated_cf32_bytes_per_second: all.filter(c => c.enabled).reduce((sum, c) => sum + (Number.isFinite(fixtureRate(c)) ? fixtureRate(c)*8 : 0), 0) };
  }
  private endSegments(uid: string, reason: string): void {
    this.state.recordings.filter(t => t.receiver_uid === uid).forEach(t => t.tracks.forEach(track => {
      const tail = track.segments[track.segments.length - 1]; if (tail && !tail.end_reason) tail.end_reason = reason;
    }));
  }
  private change(r: ReceiverState, config: ReceiverConfig, boundary: boolean): void {
    r.confirmed = clone(config); r.revision = String(BigInt(r.revision) + 1n);
    this.state.recordings.filter(t => t.receiver_uid === r.receiver_uid && ['preparing', 'recording'].includes(t.state)).forEach(task => {
      for (const track of task.tracks) if ((track.kind === 'audio' && config.purpose !== 'listen') || (track.kind === 'results' && config.purpose !== 'digital')) {
        track.state = 'completed'; const tail = track.segments[track.segments.length - 1]; if (tail && !tail.end_reason) tail.end_reason = 'processor_removed';
      }
      if (task.tracks.every(track => track.state === 'completed')) this.finish(task);
    });
    if (boundary) {
      r.clock!.actual_rate_hz = fixtureRate(config); r.clock!.output_index = '0';
      this.state.outputs.filter(o => o.receiver_uid === r.receiver_uid && o.state === 'active').forEach(o => { o.actual_rate_hz = r.clock!.actual_rate_hz; if (!config.enabled) o.state = 'ended'; });
      this.endSegments(r.receiver_uid, config.enabled ? 'baseband_change' : 'receiver_stopped');
      r.stream = config.enabled ? { source_id: r.source_id, epoch: this.state.capture.epoch, channel_id: String(this.nextId++) } : null;
      if (config.enabled) this.state.recordings.filter(t => t.receiver_uid === r.receiver_uid && ['preparing', 'recording'].includes(t.state))
        .forEach(t => t.tracks.filter(track => ['preparing', 'recording'].includes(track.state)).forEach(track => track.segments.push(this.segment(r, track.kind))));
      else this.state.recordings.filter(t => t.receiver_uid === r.receiver_uid && ['recording', 'preparing'].includes(t.state))
        .forEach(t => this.finish(t));
    }
    r.state = config.enabled ? 'running' : 'stopped'; r.fault = null;
  }
  private segment(r: ReceiverState, kind: TrackKind) {
    return { frequency_hz: r.confirmed.baseband.frequency_hz, ...(r.confirmed.processors.length ? { mode: r.confirmed.processors[0].mode } : {}), stream: clone(r.stream!), first_input_index: r.clock!.input_index, samples: '0',
      actual_rate_hz: kind === 'iq' ? r.clock!.actual_rate_hz : kind === 'audio' ? 48_000 : null, end_reason: null };
  }
  private finish(task: RecordingTask): void {
    task.tracks.forEach(track => { const tail = track.segments[track.segments.length - 1]; if (tail && !tail.end_reason) tail.end_reason = 'stop'; });
    this.taskState(task, 'finalizing');
    this.later(() => { if (task.state === 'finalizing') { this.taskState(task, 'completed'); this.emit(); } });
  }
  private taskState(task: RecordingTask, state: TaskState): void {
    task.state = state; task.tracks.forEach(track => { if (!['completed', 'failed'].includes(track.state)) track.state = state; });
  }
  async dispatch(command: ServiceCommand): Promise<CommandReply> {
    // Capture the injected behavior at ingress, so another request cannot steal it.
    const rejected = this.rejectNext; this.rejectNext = false;
    const delay = this.delayNext ? this.latency * 8 : this.latency; this.delayNext = false;
    await new Promise(resolve => setTimeout(resolve, delay));
    const fault = (code: string, message: string): CommandReply => ({ request_id: command.request_id,
      snapshot: this.snapshot(), fault: { scope: 'request', code, message, request_id: command.request_id, retryable: true } });
    if (this.disposed) return fault('CLOSED', 'Fixture adapter disposed');
    if (rejected) return fault('FIXTURE_REJECTED', 'Injected request rejection; confirmed configuration retained');
    if (command.type === 'configure' || command.type === 'scene') {
      const impact = this.preflight(command);
      if (!impact.accepted) return fault('PREFLIGHT', impact.faults.map(f => f.message).join('; '));
      if (command.type === 'scene') {
        this.state.capture.center_hz = command.capture_center_hz;
        if (impact.capture_change) this.state.capture.epoch = String(BigInt(this.state.capture.epoch) + 1n);
        for (const change of command.configs) this.change(this.state.receivers.find(r => r.receiver_uid === change.receiver_uid)!, change.config, true);
      } else this.change(this.state.receivers.find(r => r.receiver_uid === command.receiver_uid)!, command.config,
        impact.affected_receivers.includes(command.receiver_uid));
    } else if (command.type === 'record_start') {
      const r = this.state.receivers.find(r => r.receiver_uid === command.receiver_uid);
      if (!r?.stream || r.state !== 'running') return fault('NO_SOURCE', 'Receiver has no healthy stream');
      if (!command.kinds.length || new Set(command.kinds).size !== command.kinds.length
        || (r.confirmed.purpose === 'iq' && command.kinds.some(k => k !== 'iq'))
        || (r.confirmed.purpose !== 'digital' && command.kinds.includes('results'))) return fault('NO_CAPABILITY', 'Track source unavailable');
      if (this.state.recordings.some(t => t.receiver_uid === r.receiver_uid && ['preparing', 'recording', 'finalizing'].includes(t.state) && t.tracks.some(track => command.kinds.includes(track.kind)))) return fault('BUSY', 'Recording already active');
      if (!this.snapshot().recording_resources!.available_tasks) return fault('LIMIT', 'Recording task capacity reached');
      const id = `fixture-task-${this.nextId++}`;
      const task: RecordingTask = { task_id: id, receiver_uid: r.receiver_uid, state: 'preparing', fault: null,
        name: r.confirmed.name, receiver_name: r.confirmed.name, created_at_ms: String(Date.now()), ...(command.kinds.includes('audio') ? { audio_format: command.audio_format ?? 'opus' } : {}),
        tracks: command.kinds.map(kind => ({ track_id: `${id}-${kind}`, kind,
          source: kind === 'iq' ? 'baseband' : kind === 'audio' ? 'analysis_audio' : 'decoder_results',
          format: kind === 'iq' ? 'cf32_le' : kind === 'audio' ? command.audio_format ?? 'opus' : 'jsonl',
          ...(kind === 'audio' ? { container: 'websa-audio-files-v1' } : {}),
          compression: kind === 'iq' ? 'zstd_auto_lossless' : 'none', state: 'preparing',
          segments: [this.segment(r, kind)], raw_bytes: '0', stored_bytes: '0' })) };
      this.state.recordings.push(task);
      this.later(() => { if (task.state === 'preparing') { this.taskState(task, 'recording'); this.emit(); } });
    } else if (command.type === 'record_stop') {
      const task = this.state.recordings.find(t => t.task_id === command.task_id);
      if (!task || !['preparing', 'recording', 'waiting'].includes(task.state)) return fault('TASK_STATE', 'Task cannot be stopped');
      this.finish(task);
    } else if (command.type === 'record_rename' || command.type === 'record_delete') {
      const task = this.state.recordings.find(t => t.task_id === command.task_id);
      if (!task || !['completed', 'failed'].includes(task.state)) return fault('TASK_STATE', 'Stop recording before managing files');
      if (command.type === 'record_delete') this.state.recordings = this.state.recordings.filter(t => t !== task);
      else {
        const name = command.name.trim();
        if (!name || [...name].length > 128 || /[\u0000-\u001f\u007f]/.test(name)) return fault('INVALID_NAME', 'Use 1 to 128 characters');
        task.name = name;
      }
    } else if (command.type === 'output') {
      const r = this.state.receivers.find(r => r.receiver_uid === command.receiver_uid);
      if (!r) return fault('NOT_FOUND', 'Unknown receiver');
      if (command.enabled && r.state !== 'running') return fault('NO_SOURCE', 'Receiver has no healthy stream');
      this.state.outputs = this.state.outputs.filter(o => o.receiver_uid !== r.receiver_uid);
      if (command.enabled) this.state.outputs.push({ subscription_id: `fixture-output-${this.nextId++}`, receiver_uid: r.receiver_uid,
        kind: 'iq', format: 'cf32_le', actual_rate_hz: r.clock!.actual_rate_hz, state: 'active', fault: null });
    } else if (command.type === 'retire') {
      if (this.state.recordings.some(t => t.receiver_uid === command.receiver_uid && !['completed', 'failed'].includes(t.state))) return fault('BUSY', 'Stop recording before retiring definition');
      this.state.receivers = this.state.receivers.filter(r => r.receiver_uid !== command.receiver_uid);
      this.state.outputs = this.state.outputs.filter(o => o.receiver_uid !== command.receiver_uid);
    } else if (command.type === 'create') {
      if (this.state.receivers.length >= this.state.capabilities.max_channels) return fault('LIMIT', `Fixture runtime limit reached (${this.state.capabilities.max_channels}); ${this.state.capabilities.capacity_target} is a validation target`);
      const id = this.nextId;
      const r = clone(fixtureSnapshot().receivers[2]); r.receiver_uid = `fixture-c${id}`;
      r.display_number = id; r.confirmed = clone(command.config); r.stream!.channel_id = String(id);
      r.stream!.epoch = this.state.capture.epoch; r.clock!.actual_rate_hz = fixtureRate(r.confirmed);
      if (!r.confirmed.enabled) { r.state = 'stopped'; r.stream = null; }
      this.state.receivers.push(r);
      const impact = this.preflight({ type: 'configure', request_id: command.request_id, expected_revision: r.revision, receiver_uid: r.receiver_uid, config: r.confirmed });
      if (!impact.accepted) { this.state.receivers.pop(); return fault('PREFLIGHT', impact.faults.map(f => f.message).join('; ')); }
      this.nextId++;
    }
    this.emit(); return { request_id: command.request_id, snapshot: this.snapshot(), fault: null };
  }
  inject(kind: 'reject' | 'late' | 'disk' | 'gap' | 'recover' | 'consumer' | 'capture', uid: string): void {
    if (kind === 'reject') { this.rejectNext = true; return; }
    if (kind === 'late') { this.delayNext = true; return; }
    const r = this.state.receivers.find(r => r.receiver_uid === uid); if (!r) return;
    if (kind === 'disk') {
      this.state.recordings.filter(t => t.receiver_uid === uid && ['recording', 'preparing', 'finalizing'].includes(t.state)).forEach(t => {
        this.taskState(t, 'failed'); t.fault = { scope: 'recording', code: 'FIXTURE_DISK_FULL', message: 'Injected disk full', task_id: t.task_id, retryable: true };
      });
      this.endSegments(uid, 'disk_failure');
    } else if (kind === 'consumer') {
      this.state.outputs.filter(o => o.receiver_uid === uid).forEach(o => {
        o.state = 'failed'; o.fault = { scope: 'consumer', code: 'SLOW', message: 'Injected slow consumer', retryable: true };
      });
    } else if (kind === 'recover') {
      if (this.state.capture.state === 'failed') this.state.capture.epoch = String(BigInt(this.state.capture.epoch) + 1n);
      this.state.capture.state = 'running';
      this.state.receivers.filter(c => c.confirmed.enabled && ['failed', 'interrupted'].includes(c.state)).forEach(c => this.change(c, c.confirmed, true));
    } else {
      if (kind === 'capture') this.state.capture.state = 'failed';
      this.state.receivers.filter(c => kind === 'capture' || c.receiver_uid === uid).forEach(c => {
        c.state = kind === 'capture' ? 'failed' : 'interrupted';
        c.fault = { scope: kind === 'capture' ? 'capture' : 'receiver', code: 'FIXTURE_GAP', message: 'Injected stream interruption', retryable: true };
        this.endSegments(c.receiver_uid, 'gap'); c.stream = null;
        this.state.outputs.filter(o => o.receiver_uid === c.receiver_uid && o.state === 'active').forEach(o => { o.state = 'failed'; o.fault = clone(c.fault); });
        this.state.recordings.filter(t => t.receiver_uid === c.receiver_uid && ['recording', 'preparing'].includes(t.state)).forEach(t => this.finish(t));
      });
    }
    this.emit();
  }
}
