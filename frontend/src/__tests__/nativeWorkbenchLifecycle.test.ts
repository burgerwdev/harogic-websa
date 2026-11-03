import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountNativeWorkbenchStreams } from '../ui/nativeWorkbenchStreams';
import { decoderLog, forgetDecoderLog } from '../sdr/decoderLog';
import { forgetNativeAudio, setNativeAudioRoute } from '../audio/sdrAudio';
import { send, setWS } from '../core/wsSend';

vi.mock('../audio/sdrAudio', () => ({ claimNativeAudio: () => vi.fn(), forgetNativeAudio: vi.fn(),
  getSdrAudioStatus: () => [], setNativeAudioRoute: vi.fn(), setNativeMaster: vi.fn() }));
class Worker {
  static instances: Worker[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  postMessage = vi.fn(); terminate = vi.fn();
  constructor() { Worker.instances.push(this); }
  result(service: string | undefined = undefined): void {
    this.onmessage?.({ data: { type: 'frames', envelopes: [{ version: 1, channel: 1, channel_id: 8, epoch: 8, seq: 1,
      receiver_uid: 'fixture-c1', service_id: service, input_index: '9007199254740999', input_rate: 62500000,
      input_fraction_num: '0', input_fraction_den: '1', delay_seconds: 0, delay_known: true, causal_presentation: false,
      mode: 'cw', event: { type: 'cw', text: 'TAIL' } }] } });
  }
}
let adapter: FixtureReceiverAdapter, session: WorkbenchSession, streams: ReturnType<typeof mountNativeWorkbenchStreams>;
beforeEach(() => {
  Worker.instances = []; vi.stubGlobal('Worker', Worker);
  adapter = new FixtureReceiverAdapter(); session = new WorkbenchSession(adapter); streams = mountNativeWorkbenchStreams(session);
});
afterEach(() => {
  streams.dispose(); session.dispose(); adapter.dispose();
  for (const uid of ['fixture-c1', 'fixture-c2', 'fixture-c3']) forgetDecoderLog(uid);
  vi.unstubAllGlobals(); vi.clearAllMocks();
});
describe('UID consumer lifecycle', () => {
  it('publishes decoder updates independently of receiver controls and speaker routing', () => {
    const stateChanged = vi.fn(), resultsChanged = vi.fn();
    session.subscribe(stateChanged); session.subscribeResults(resultsChanged);
    vi.mocked(setNativeAudioRoute).mockClear();
    Worker.instances[0].result(); Worker.instances[0].result();
    expect(resultsChanged).toHaveBeenCalledTimes(2);
    expect(resultsChanged).toHaveBeenCalledWith('fixture-c1');
    expect(stateChanged).not.toHaveBeenCalled();
    expect(setNativeAudioRoute).not.toHaveBeenCalled();
    expect(Worker.instances[0].postMessage).toHaveBeenCalledWith({ type: 'ack' });
  });
	it('restores C1 monitoring on an explicit main-panel restart after all receivers stop', () => {
		session.snapshot = clone(session.snapshot);
		for (const receiver of session.snapshot.receivers) { receiver.confirmed.enabled = false; receiver.state = 'stopped'; }
		session.monitorMode = 'follow'; session.select('fixture-c2');
		session.masterMuted = true; session.masterGain = 0.35;
		session.monitor('fixture-c1', { gain: 0.4, pan: 0.6 });
		const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket;
		setWS(socket);
		try {
			// Shared capture edits and PCM gain are independent of starting reception.
			send({ cmd: 'SET_SDR', center: 7.1e6 });
			send({ cmd: 'SET_SDR_CHANNEL', channel: 1, volume: 0.2 });
			expect(session.route('fixture-c1').enabled).toBe(false);
			send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.1e6, enabled: true });
			expect(session.route('fixture-c1')).toEqual({ enabled: true, gain: 0.4, pan: 0.6, solo: false });
			expect(session.monitorMode).toBe('manual');
			expect(session.masterMuted).toBe(true); expect(session.masterGain).toBe(0.35);
			expect(session.audible('fixture-c1')).toBe(false);
			session.snapshot.receivers[0].confirmed.enabled = true;
			session.snapshot.receivers[0].state = 'running'; session.notify();
			expect(setNativeAudioRoute).toHaveBeenCalledWith('fixture-c1', 'nfm', true, 0.4, 0.6, false);
			// Repeated live tuning must preserve a later manual monitoring choice.
			session.monitor('fixture-c1', { enabled: false });
			send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6 });
			expect(session.route('fixture-c1').enabled).toBe(false);
			streams.dispose();
			for (const receiver of session.snapshot.receivers) { receiver.confirmed.enabled = false; receiver.state = 'stopped'; }
			send({ cmd: 'SET_SDR', center: 7.1e6, listen: 7.1e6 });
			expect(session.route('fixture-c1').enabled).toBe(false);
		} finally { setWS(null); }
	});

  it('retires the worker before releasing its history and rejects queued callbacks after slot reuse', () => {
    const old = Worker.instances[0], healthy = Worker.instances[1]; old.result();
    expect(decoderLog('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
    session.snapshot = clone(session.snapshot); session.snapshot.receivers.shift(); session.notify();
    expect(old.terminate).toHaveBeenCalledOnce(); expect(healthy.terminate).not.toHaveBeenCalled();
    expect(forgetNativeAudio).toHaveBeenCalledWith('fixture-c1');
    old.result(); expect(decoderLog('fixture-c1').entries).toHaveLength(0);
    expect(streams.results('fixture-c1').entries).toHaveLength(0);
  });
  it('clears source-scoped caches and ignores old callbacks even if a new service reuses a UID', () => {
    const old = Worker.instances[0]; old.result();
    session.snapshot = { ...clone(session.snapshot), service_id: 'service-B' }; session.notify();
    expect(old.terminate).toHaveBeenCalledOnce(); old.result();
    expect(decoderLog('fixture-c1').entries).toHaveLength(0);
    Worker.instances[2].result('service-B'); expect(streams.results('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
  });
  it('accepts the stopped source FIFO tail, then releases its worker without starting another', () => {
    const old = Worker.instances[0];
    session.snapshot = clone(session.snapshot); session.snapshot.receivers[0].state = 'stopped'; session.notify();
    expect(old.terminate).not.toHaveBeenCalled(); old.result();
    expect(streams.results('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
    old.onmessage?.({ data: { type: 'closed', message: 'stopped' } });
    expect(old.terminate).toHaveBeenCalledOnce(); expect(Worker.instances).toHaveLength(2);
    old.result(); expect(streams.results('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
  });
});
