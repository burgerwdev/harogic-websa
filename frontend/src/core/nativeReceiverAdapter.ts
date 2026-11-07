import { clone, type CommandReply, type ConfigureCommand, type Fault, type Impact,
  type OutputSubscription, type ReceiverAdapter, type SceneCommand, type ServiceCommand,
  type ServiceSnapshot, type SourceClock, type StreamIdentity } from './multichannel';

export interface ReceivedIq { receiver_uid: string; stream: StreamIdentity; clock: SourceClock; sequence: number; iq: Float32Array }
const api = '/api/sdr/receivers';
const fault = (scope: Fault['scope'], code: string, message: string): Fault => ({ scope, code, message, retryable: true });

/** Shared native control/data owner. Views subscribe to this object; closing a view sends no command. */
export class NativeReceiverAdapter implements ReceiverAdapter {
  private listeners = new Set<(snapshot: ServiceSnapshot) => void>();
  private iqListeners = new Set<(block: ReceivedIq) => void>();
  private sockets = new Map<string, WebSocket>();
  private outputs = new Map<string, OutputSubscription>();
  private clock = new Map<string, { stream: StreamIdentity; clock: SourceClock }>();
  private timer: ReturnType<typeof setInterval>;
  private refreshing = false;
  private disposed = false;
  private readonly lifetime = new AbortController();
  private retiredServices = new Set<string | undefined>();
  private client = crypto.randomUUID();
  private constructor(private current: ServiceSnapshot) {
    this.timer = setInterval(() => { void this.refresh(); }, 500);
  }
  static async connect(): Promise<NativeReceiverAdapter> {
    const response = await fetch(api, { signal: AbortSignal.timeout(8000) });
    if (!response.ok) throw new Error(`Receiver service: HTTP ${response.status}`);
    const snapshot = await response.json() as ServiceSnapshot;
    if (snapshot.transport !== 'native' || snapshot.contract !== 'multichannel-draft-v1') throw new Error('Unsupported receiver service');
    return new NativeReceiverAdapter(snapshot);
  }
  snapshot(): ServiceSnapshot { return clone({ ...this.current, outputs: [...this.outputs.values()] }); }
  subscribe(listener: (snapshot: ServiceSnapshot) => void): () => void {
    this.listeners.add(listener); return () => this.listeners.delete(listener);
  }
  onIq(listener: (block: ReceivedIq) => void): () => void {
    this.iqListeners.add(listener); return () => this.iqListeners.delete(listener);
  }
  private notify(): void { if (!this.disposed) this.listeners.forEach(listener => listener(this.snapshot())); }
  private accept(snapshot: ServiceSnapshot): void {
    if (this.disposed) return;
    if (this.retiredServices.has(snapshot.service_id)) return;
    if (snapshot.service_id === this.current.service_id && BigInt(snapshot.revision) < BigInt(this.current.revision)) return;
    if (snapshot.service_id === this.current.service_id && snapshot.state_revision !== undefined && this.current.state_revision !== undefined
      && BigInt(snapshot.state_revision) < BigInt(this.current.state_revision)) return;
    if (snapshot.service_id !== this.current.service_id) {
      this.retiredServices.add(this.current.service_id);
      this.sockets.forEach(socket => socket.close()); this.sockets.clear(); this.clock.clear();
      this.outputs.forEach(output => { output.state = 'failed'; output.fault = fault('consumer', 'SOURCE_RESTARTED', 'Source service restarted; subscribe again'); });
    }
    this.current = snapshot; this.notify();
  }
  async refresh(): Promise<void> {
    if (this.refreshing || this.disposed) return;
    this.refreshing = true;
    const service = this.current.service_id;
    try {
      const response = await fetch(api, { signal: AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(8000)]) });
      if (response.ok) {
        const snapshot = await response.json() as ServiceSnapshot;
        if (service === this.current.service_id) this.accept(snapshot);
      }
    } catch { /* Keep the last confirmed device state; never replay configuration after a network failure. */ }
    finally { this.refreshing = false; }
  }
  /** Local impact preview; native resource admission is performed again by the service on dispatch. */
  preflight(command: ConfigureCommand | SceneCommand): Impact {
    const config = command.type === 'configure' ? command.config : null;
    const faults: Fault[] = [];
    if (!config) faults.push(fault('request', 'UNAVAILABLE', 'Atomic scenes are not available from this service'));
    else {
      const b = config.baseband, c = this.current.capture;
      if (![b.frequency_hz, b.passband_hz, b.transition_hz, b.minimum_rate_hz].every(value => Number.isFinite(value) && value > 0))
        faults.push(fault('request', 'INVALID_BASEBAND', 'Baseband requirements must be finite positive values'));
      if (config.enabled && (c.state !== 'running' || c.usable_bandwidth_hz === null || c.parameters_state === 'unknown'))
        faults.push(fault('request', 'NO_SOURCE', 'Start or recover the shared capture first'));
      else if (config.enabled && Math.abs(b.frequency_hz - c.center_hz) + b.passband_hz / 2 + b.transition_hz > c.usable_bandwidth_hz! / 2)
        faults.push(fault('request', 'RF_RANGE', 'Full passband and transition exceed the shared capture'));
    }
    return { accepted: !faults.length, faults, capture_change: false, affected_receivers: command.type === 'configure' ? [command.receiver_uid] : [],
      affected_tracks: command.type === 'configure' ? this.current.recordings.filter(task => task.receiver_uid === command.receiver_uid && ['preparing', 'recording', 'finalizing'].includes(task.state)).flatMap(task => task.tracks.map(track => track.track_id)) : [], candidate_center_hz: this.current.capture.center_hz,
      estimated_cf32_bytes_per_second: config ? config.baseband.minimum_rate_hz * 8 : 0 };
  }
  async dispatch(command: ServiceCommand): Promise<CommandReply> {
    if (this.disposed) throw new Error('Receiver adapter is closed');
    if (command.type === 'output') {
      this.output(command.receiver_uid, command.enabled);
      return { request_id: command.request_id, snapshot: this.snapshot(), fault: null };
    }
    if (!this.current.capabilities.commands?.includes(command.type)) return { request_id: command.request_id, snapshot: this.snapshot(),
      fault: fault('request', 'UNAVAILABLE', 'This operation is not available from the native receiver service') };
    const service = this.current.service_id;
    const request = { ...command, service_id: service, request_id: `${this.client}:${command.request_id}` };
    const options: RequestInit = { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request),
      signal: AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(8000)]) };
    if (!command.type.startsWith('record_')) {
    const checked = await fetch(`${api}/preflight`, options);
    if (!checked.ok) throw new Error(`Receiver preflight: HTTP ${checked.status}`);
    const impact = await checked.json() as Impact;
    if (this.disposed) throw new Error('Receiver adapter closed during admission');
    if (service !== this.current.service_id) throw new Error('Source service restarted during admission');
    if (!impact.accepted) return { request_id: command.request_id, snapshot: this.snapshot(), fault: impact.faults[0] };
    }
    const response = await fetch(api, { ...options, signal: AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(8000)]) });
    if (!response.ok) throw new Error(`Receiver command: HTTP ${response.status}`);
    const reply = await response.json() as CommandReply;
    if (this.disposed) throw new Error('Receiver adapter closed during command');
    if (reply.request_id !== request.request_id) throw new Error('Receiver response request_id mismatch');
    if (service !== this.current.service_id || reply.snapshot.service_id !== service) throw new Error('Source service restarted during command');
    this.accept(reply.snapshot);
    return { ...reply, request_id: command.request_id, snapshot: this.snapshot() };
  }
  private output(uid: string, enabled: boolean): void {
    if (!enabled) { this.sockets.get(uid)?.close(); this.sockets.delete(uid); const output = this.outputs.get(uid); if (output) output.state = 'ended'; this.notify(); return; }
    if (this.sockets.has(uid)) return;
    const receiver = this.current.receivers.find(receiver => receiver.receiver_uid === uid);
    if (!receiver?.confirmed.enabled) throw new Error('Enable the receiver before subscribing to IQ');
    const url = new URL('/ws/iq', location.href); url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'; url.searchParams.set('receiver_uid', uid);
    const socket = new WebSocket(url); socket.binaryType = 'arraybuffer'; this.sockets.set(uid, socket);
    const output: OutputSubscription = { subscription_id: `${this.client}:${uid}`, receiver_uid: uid, kind: 'iq', format: 'cf32_le',
      actual_rate_hz: null, state: 'starting', fault: null, samples: '0', bytes: '0' };
    this.outputs.set(uid, output); this.notify();
    const failed = (message: string): void => { output.state = 'failed'; output.fault = fault('consumer', 'IQ_STREAM', message); socket.close(); this.notify(); };
    socket.onmessage = event => {
      if (this.sockets.get(uid) !== socket || this.disposed) return;
      try {
        if (typeof event.data === 'string') {
          const message = JSON.parse(event.data);
          if (message.type === 'stream_start') {
            if (message.receiver_uid !== uid || !Number.isFinite(message.clock?.actual_rate_hz) || message.clock.actual_rate_hz <= 0) throw new Error('Invalid IQ start metadata');
            this.clock.set(uid, { stream: message.stream, clock: message.clock }); output.actual_rate_hz = message.clock.actual_rate_hz; output.state = 'active';
          } else if (message.type === 'stream_boundary' && message.next === null) {
            output.state = message.drained === true ? 'ended' : 'failed';
            if (output.state === 'failed') output.fault = fault('consumer', 'SOURCE_BOUNDARY', message.reason);
          }
        } else if (event.data instanceof ArrayBuffer) {
          const data = event.data, header = new DataView(data), clock = this.clock.get(uid);
          const magic = data.byteLength >= 4 ? String.fromCharCode(...new Uint8Array(data, 0, 4)) : '';
          if (!clock || data.byteLength < 56 || !['IQBF', 'IQAX'].includes(magic) || header.getUint32(4, true) !== 2
            || data.byteLength !== 56 + header.getUint32(12, true) * 8 || header.getFloat64(16, true) !== clock.clock.actual_rate_hz
            || header.getUint32(32, true).toString() !== clock.stream.channel_id || header.getUint32(36, true).toString() !== clock.stream.epoch) throw new Error('IQ identity or payload length mismatch');
          const samples = header.getUint32(12, true);
          output.samples = (BigInt(output.samples!) + BigInt(samples)).toString(); output.bytes = (BigInt(output.bytes!) + BigInt(data.byteLength)).toString();
          const input = header.getBigUint64(40, true), step = Math.round(clock.clock.input_rate_hz / clock.clock.actual_rate_hz);
          const position = { ...clock.clock, input_index: input.toString(), output_index: (BigInt(clock.clock.output_index)
            + (input - BigInt(clock.clock.input_index)) / BigInt(Math.max(1, step))).toString() };
          this.iqListeners.forEach(listener => {
            try { listener({ receiver_uid: uid, stream: clone(clock.stream), clock: position, sequence: header.getUint32(8, true), iq: new Float32Array(data, 56) }); }
            catch (error) { console.error('IQ observer failed', error); }
          });
        }
        // Sample counters repaint at the shared snapshot cadence, rather than
        // rebuilding every view for every block from every receiver.
        if (typeof event.data === 'string') this.notify();
      } catch (error) { failed(String(error)); }
    };
    socket.onerror = () => failed('IQ subscription failed');
    socket.onclose = () => {
      if (this.sockets.get(uid) !== socket) return;
      this.sockets.delete(uid);
      if (!this.disposed && (output.state === 'starting' || output.state === 'active')) {
        output.state = 'failed'; output.fault = fault('consumer', 'DISCONNECTED', 'IQ disconnected without an end boundary');
      }
      this.notify();
    };
  }
  dispose(): void {
    this.disposed = true; this.lifetime.abort(); clearInterval(this.timer); this.sockets.forEach(socket => socket.close()); this.sockets.clear(); this.listeners.clear(); this.iqListeners.clear();
  }
}
