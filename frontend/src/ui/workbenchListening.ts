import { clone, type ConfigureCommand, type Fault, type ReceiverConfig, type ServiceCommand, type U64 } from '../core/multichannel';
import type { WorkbenchSession } from './workbenchSession';

export const LISTEN_MODES = ['nfm', 'am', 'usb'] as const;
export interface ListenRequest { frequency_hz: number; target: string; mode: string }
export interface ListenPlan {
  request: ListenRequest; service_revision: U64; service_id?: string; lifecycle_version: number;
  command: ConfigureCommand | Extract<ServiceCommand, { type: 'create' }> | null;
  receiver_uid: string | null; affected_tracks: string[]; fault: Fault | null;
}
const fault = (code: string, message: string): Fault => ({ scope: 'request', code, message, retryable: true });
export function defaultListenTarget(session: WorkbenchSession, hz: number): string {
  const receivers = session.snapshot.receivers.filter(r => r.confirmed.purpose === 'listen');
  return receivers.find(r => r.confirmed.baseband.frequency_hz === hz)?.receiver_uid
    ?? (session.snapshot.receivers.length < session.snapshot.capabilities.max_channels ? 'new'
      : receivers.find(r => r.receiver_uid === session.selected)?.receiver_uid ?? receivers[0]?.receiver_uid ?? 'new');
}
/** Prepare one explicit listen target; digital/IQ receivers are never repurposed implicitly. */
export function planListen(session: WorkbenchSession, request: ListenRequest): ListenPlan {
  const snapshot = session.snapshot;
  const plan: ListenPlan = { request: { ...request }, service_revision: snapshot.revision,
    service_id: snapshot.service_id, lifecycle_version: session.lifecycleVersion,
    command: null, receiver_uid: null, affected_tracks: [], fault: null };
  const reject = (code: string, message: string): ListenPlan => { plan.fault = fault(code, message); return plan; };
  if (!Number.isFinite(request.frequency_hz) || request.frequency_hz <= 0) return reject('FREQUENCY', 'Enter a positive frequency');
  if (!(LISTEN_MODES as readonly string[]).includes(request.mode) || !snapshot.capabilities.modes.includes(request.mode)) return reject('MODE', 'Listening mode unavailable');
  if (snapshot.capture.state !== 'running' || snapshot.capture.usable_bandwidth_hz === null
    || snapshot.capture.parameters_state === 'unknown') return reject('NO_SOURCE', 'Recover the capture source first');
  const receiver = snapshot.receivers.find(r => r.receiver_uid === request.target);
  if (request.target !== 'new' && receiver?.confirmed.purpose !== 'listen') return reject('TARGET', 'Choose a listening receiver');
  if (receiver && session.edits.get(receiver.receiver_uid)?.pending) return reject('BUSY', 'Receiver configuration pending');
  if (receiver && ['interrupted', 'failed'].includes(receiver.state)) return reject('NO_SOURCE', 'Recover this receiver first');
  if (!receiver && snapshot.receivers.length >= snapshot.capabilities.max_channels) return reject('LIMIT', 'Channel limit reached; choose an existing listening receiver');
  const config: ReceiverConfig = receiver ? clone(receiver.confirmed) : {
    name: `${request.frequency_hz / 1e6} MHz`, color: '#72dd91', tags: [], enabled: true, purpose: 'listen',
    baseband: { frequency_hz: request.frequency_hz, passband_hz: 12_500, transition_hz: 1_000, minimum_rate_hz: 6_000 }, processors: [],
  };
  const oldMode = config.processors.find(p => p.kind === 'demodulator')?.mode;
  config.enabled = true; config.baseband.frequency_hz = request.frequency_hz;
  if (oldMode !== request.mode) {
    config.baseband.passband_hz = request.mode === 'usb' ? 3_000 : request.mode === 'am' ? 10_000 : 12_500;
    config.baseband.transition_hz = request.mode === 'usb' ? 500 : 1_000;
    config.processors = [{ id: `listen-${session.requestId()}`, kind: 'demodulator', mode: request.mode, minimum_rate_hz: 0 }];
  }
  if (Math.abs(request.frequency_hz - snapshot.capture.center_hz) + config.baseband.passband_hz / 2 + config.baseband.transition_hz > snapshot.capture.usable_bandwidth_hz / 2) return reject('RF_RANGE', 'Full passband does not fit the shared capture; adjust capture explicitly');
  if (receiver) {
    plan.receiver_uid = receiver.receiver_uid;
    if (JSON.stringify(config) === JSON.stringify(receiver.confirmed) && receiver.state === 'running') return plan;
    const command: ConfigureCommand = { type: 'configure', request_id: session.requestId(), receiver_uid: receiver.receiver_uid,
      expected_revision: receiver.revision, config };
    const impact = session.adapter.preflight(command);
    if (!impact.accepted) { plan.fault = impact.faults[0]; return plan; }
    if (impact.affected_receivers.includes(receiver.receiver_uid)) plan.affected_tracks = snapshot.recordings
      .filter(t => t.receiver_uid === receiver.receiver_uid && ['preparing', 'waiting', 'recording'].includes(t.state))
      .flatMap(t => t.tracks.filter(track => !['completed', 'failed'].includes(track.state)).map(track => track.track_id));
    plan.command = command;
  } else plan.command = { type: 'create', request_id: session.requestId(), config };
  return plan;
}
const applying = new WeakSet<WorkbenchSession>();
/** Enable the local route only after the service has confirmed this exact request. */
export async function applyListen(session: WorkbenchSession, plan: ListenPlan): Promise<{ receiver_uid: string | null; fault: Fault | null }> {
  const reject = (error: Fault) => ({ receiver_uid: null, fault: error });
  if (plan.fault) return reject(plan.fault);
  if (applying.has(session)) return reject(fault('BUSY', 'Listening request pending'));
  const current = (): boolean => plan.service_id === session.snapshot.service_id && plan.lifecycle_version === session.lifecycleVersion;
  if (!current() || plan.service_revision !== session.snapshot.revision) return reject(fault('STALE_PREVIEW', 'State changed; review the updated preview'));
  applying.add(session);
  try {
    let uid = plan.receiver_uid;
    if (plan.command) {
      if (plan.command.type === 'configure') {
        if (session.edits.get(plan.command.receiver_uid)?.pending) return reject(fault('BUSY', 'Receiver configuration pending'));
        await session.configure(plan.command.receiver_uid, plan.command.config);
        const error = session.edits.get(plan.command.receiver_uid)?.fault;
        if (error) return reject(error);
      } else {
        const reply = await session.create(plan.command.config);
        if (reply.fault) return reject(reply.fault);
        uid = reply.receiver_uid;
      }
    }
    if (!current()) return reject(fault('STALE_PREVIEW', 'State changed before listening was enabled'));
    const confirmed = session.snapshot.receivers.find(r => r.receiver_uid === uid);
    if (!confirmed || session.retiring(confirmed.receiver_uid) || confirmed.state !== 'running' || confirmed.confirmed.purpose !== 'listen'
      || confirmed.confirmed.baseband.frequency_hz !== plan.request.frequency_hz
      || !confirmed.confirmed.processors.some(p => p.kind === 'demodulator' && p.mode === plan.request.mode)) return reject(fault('CONFIRMATION', 'Receiver changed before listening was enabled'));
    session.monitorMode = 'manual'; session.monitor(confirmed.receiver_uid, { enabled: true });
    return { receiver_uid: confirmed.receiver_uid, fault: null };
  } catch (error) {
    return reject(fault('REQUEST_FAILED', String(error)));
  } finally { applying.delete(session); }
}
