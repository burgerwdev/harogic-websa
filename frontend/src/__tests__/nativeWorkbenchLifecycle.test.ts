import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountNativeWorkbenchStreams } from '../ui/nativeWorkbenchStreams';
import { decoderLog, forgetDecoderLog } from '../sdr/decoderLog';
import { forgetNativeAudio, getSdrAudioStatus, sampleNativeAudioWaveform, setNativeAudioRoute } from '../audio/sdrAudio';
import { publishCommandReply, send, setWS } from '../core/wsSend';

vi.mock('../audio/sdrAudio', () => ({ claimNativeAudio: () => vi.fn(), forgetNativeAudio: vi.fn(),
  getSdrAudioStatus: vi.fn(() => []), sampleNativeAudioWaveform: vi.fn(() => false), setNativeAudioRoute: vi.fn(), setNativeMaster: vi.fn() }));
class Worker {
  static instances: Worker[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
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
  vi.mocked(getSdrAudioStatus).mockReset().mockReturnValue([]); vi.mocked(sampleNativeAudioWaveform).mockReset().mockReturnValue(false);
  Worker.instances = []; vi.stubGlobal('Worker', Worker);
  adapter = new FixtureReceiverAdapter(); session = new WorkbenchSession(adapter); streams = mountNativeWorkbenchStreams(session);
});
afterEach(() => {
  streams.dispose(); session.dispose(); adapter.dispose();
  for (const uid of ['fixture-c1', 'fixture-c2', 'fixture-c3']) forgetDecoderLog(uid);
  vi.unstubAllGlobals(); vi.clearAllMocks();
});
describe('UID consumer lifecycle', () => {
  it('shares waveform and level readouts for duplicate audio without releasing a visible sibling probe', () => {
    session.snapshot = clone(session.snapshot);
    const [first, second] = session.snapshot.receivers;
    second.confirmed = clone(first.confirmed);
    for (const r of [first, second]) session.routes.set(r.receiver_uid, { enabled: true, gain: .7, pan: 0, solo: false });
    session.notify();
    const playback = { channel: first.receiver_uid, enabled: true, playing: true, rms: .2, peak: .4 };
    vi.mocked(getSdrAudioStatus).mockReturnValue([playback]);
    vi.mocked(sampleNativeAudioWaveform).mockImplementation((_, samples) => !!samples);
    const a = new Float32Array(256), b = new Float32Array(256);
    expect(streams.playback(second.receiver_uid)).toBe(playback);
    expect(streams.waveform(first.receiver_uid, a)).toBe(true); expect(streams.waveform(second.receiver_uid, b)).toBe(true);
    expect(sampleNativeAudioWaveform).toHaveBeenCalledWith(first.receiver_uid, b);
    vi.mocked(sampleNativeAudioWaveform).mockClear();
    streams.waveform(first.receiver_uid, null); expect(sampleNativeAudioWaveform).not.toHaveBeenCalled();
    expect(streams.waveform(second.receiver_uid, b)).toBe(true);
    vi.mocked(sampleNativeAudioWaveform).mockClear();
    streams.waveform(second.receiver_uid, null); expect(sampleNativeAudioWaveform).toHaveBeenCalledExactlyOnceWith(first.receiver_uid, null);
  });
  it('rebinds duplicate waveform consumers when the shared playback owner is disabled', () => {
    session.snapshot = clone(session.snapshot);
    const [first, second] = session.snapshot.receivers; second.confirmed = clone(first.confirmed);
    for (const r of [first, second]) session.routes.set(r.receiver_uid, { enabled: true, gain: .7, pan: 0, solo: false });
    session.notify(); vi.mocked(sampleNativeAudioWaveform).mockImplementation((_, samples) => !!samples);
    const samples = new Float32Array(256); streams.waveform(first.receiver_uid, samples); streams.waveform(second.receiver_uid, samples);
    vi.mocked(sampleNativeAudioWaveform).mockClear(); session.monitor(first.receiver_uid, { enabled: false });
    expect(sampleNativeAudioWaveform).toHaveBeenCalledExactlyOnceWith(first.receiver_uid, null);
    expect(streams.waveform(second.receiver_uid, samples)).toBe(true); expect(sampleNativeAudioWaveform).toHaveBeenLastCalledWith(second.receiver_uid, samples);
  });
  it.each(['receiver retired', 'receiver stopped'])('holds ended %s until confirmed ownership changes', reason => {
    const old = Worker.instances[0], neighbor = Worker.instances[1];
    old.result();
    const text = clone(decoderLog('fixture-c1').entries.filter(entry => entry.kind === 'text'));
    old.onmessage!({ data: { type: 'ended', reason } });
    expect(old.terminate).toHaveBeenCalledOnce();
    expect(streams.status('fixture-c1')).toBe('');
    expect(streams.results('fixture-c1').source).toBeNull();
    const count = Worker.instances.length;
    session.notify();
    expect(Worker.instances).toHaveLength(count);
    old.result(); old.onmessage!({ data: { type: 'error', message: 'late error' } });
    old.onerror!({ message: 'late terminated worker error' });
    expect(decoderLog('fixture-c1').entries.filter(entry => entry.kind === 'text')).toEqual(text);
    expect(decoderLog('fixture-c1').entries.some(entry => entry.kind === 'status')).toBe(false);
    expect(streams.status('fixture-c1')).toBe('');
    expect(neighbor.terminate).not.toHaveBeenCalled();
    session.snapshot = clone(session.snapshot);
    session.snapshot.receivers[0].state = 'stopped'; session.notify();
    session.snapshot.receivers[0].state = 'running'; session.notify();
    expect(Worker.instances).toHaveLength(count + 1);
    old.onmessage!({ data: { type: 'ended', reason } });
    expect(Worker.instances.at(-1)!.terminate).not.toHaveBeenCalled();
    expect(old.terminate).toHaveBeenCalledOnce();
  });
  it.each(['manual', 'follow'] as const)('keeps selected %s playback consuming before local gain while retaining output mute', mode => {
    session.monitorMode = mode; session.select('fixture-c1');
    session.routes.set('fixture-c1', { ...session.route('fixture-c1'), enabled: true });
    session.notify();
    for (const [localGain, masterGain] of [[0, 1], [0.7, 0]]) {
      session.routes.set('fixture-c1', { ...session.route('fixture-c1'), gain: localGain });
      session.masterGain = masterGain;
      vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
      expect(session.audible('fixture-c1')).toBe(false);
      expect(setNativeAudioRoute).toHaveBeenCalledWith('fixture-c1', 'nfm', true, localGain, 0, false);
      expect(setNativeAudioRoute).not.toHaveBeenCalledWith('fixture-c2', expect.anything(), true, expect.anything(), expect.anything(), expect.anything());
    }
    session.routes.set('fixture-c1', { ...session.route('fixture-c1'), gain: 0.7 }); session.masterGain = 1;
    vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
    expect(session.audible('fixture-c1')).toBe(true);
    expect(setNativeAudioRoute).toHaveBeenCalledWith('fixture-c1', 'nfm', true, 0.7, 0, false);
    session.snapshot = clone(session.snapshot); session.snapshot.receivers[0].state = 'stopped';
    vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
    expect(setNativeAudioRoute).toHaveBeenCalledWith('fixture-c1', 'nfm', false, 0.7, 0, false);
  });

  it('keeps one muted consumer for duplicate sources and releases the old UID after selection and retirement', () => {
    session.snapshot = clone(session.snapshot);
    const [first, second] = session.snapshot.receivers;
    second.confirmed = clone(first.confirmed); second.confirmed.processors[0].id = 'independent-processor';
    for (const receiver of [first, second])
      session.routes.set(receiver.receiver_uid, { enabled: true, gain: 0, pan: 0, solo: false });
    session.monitorMode = 'manual';
    vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
    expect(vi.mocked(setNativeAudioRoute).mock.calls.filter(call => call[2]).map(call => call[0])).toEqual(['fixture-c1']);
    session.monitorMode = 'follow'; session.selected = 'fixture-c2';
    vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
    expect(vi.mocked(setNativeAudioRoute).mock.calls.filter(call => call[2]).map(call => call[0])).toEqual(['fixture-c2']);
    session.snapshot.receivers = session.snapshot.receivers.filter(r => r.receiver_uid !== 'fixture-c2');
    vi.mocked(setNativeAudioRoute).mockClear(); session.notify();
    expect(forgetNativeAudio).toHaveBeenCalledWith('fixture-c2');
    expect(vi.mocked(setNativeAudioRoute).mock.calls.filter(call => call[2])).toEqual([]);
  });

  it('restarts only the recovered UID when its processing generation changes with the same IQ source', () => {
    session.snapshot = clone(session.snapshot);
    const receiver = session.snapshot.receivers[0];
    receiver.processing_state = 'active';
    receiver.processing_runtime = { state: 'active', processing_generation: '4', oldest_input_wait_ns: 0,
      input_held_bytes: 0, input_held_media_ns: 0, fault: null };
    session.monitor('fixture-c1', { enabled: true });
    const old = Worker.instances.at(-1)!, neighbor = Worker.instances[1];
    const iq = clone(receiver.stream);
    old.onmessage!({ data: { type: 'error', message: 'subscription retries exhausted' } });
    const count = Worker.instances.length;
    session.notify(); expect(Worker.instances).toHaveLength(count);
    receiver.processing_runtime.processing_generation = '5';
    session.notify();
    expect(receiver.stream).toEqual(iq);
    expect(Worker.instances).toHaveLength(count + 1);
    expect(streams.status('fixture-c1')).toBe('');
    expect(neighbor.terminate).not.toHaveBeenCalled();
    expect(setNativeAudioRoute).toHaveBeenCalledWith('fixture-c1', 'nfm', true, 0.7, 0, true);
    old.onmessage!({ data: { type: 'error', message: 'obsolete callback' } });
    expect(streams.status('fixture-c1')).toBe('');
  });

  it('keeps retrying subscription errors local and clears them only after validated delivery', () => {
    const worker = Worker.instances[0];
    worker.onmessage!({ data: { type: 'closed', message: 'receiver failed; reconnecting' } });
    expect(streams.status('fixture-c1')).toContain('reconnecting');
    expect(streams.status('fixture-c2')).toBe('');
    expect(worker.terminate).not.toHaveBeenCalled();
    worker.onmessage!({ data: { type: 'connected' } });
    expect(streams.status('fixture-c1')).toContain('reconnecting');
    worker.result();
    expect(streams.status('fixture-c1')).toBe('');
    expect(worker.postMessage).toHaveBeenCalledWith({ type: 'ack' });
  });

  it.each([
    { cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true },
    { cmd: 'SET_SDR_CHANNEL', channel: 1, mode: 'nfm', enabled: true },
    { cmd: 'SET_SDR', center: 7.11e6, listen: 7.11e6 },
  ])('restores audio on the newly confirmed UID after deleting every channel: $cmd', command => {
    const recreated = clone(session.snapshot.receivers[0]);
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    session.routes.clear(); session.masterGain = 0.35; session.masterMuted = true;
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket;
    setWS(socket);
    try {
      send(command);
      const request = JSON.parse(String(vi.mocked(socket.send).mock.calls[0][0]));
      publishCommandReply({ cmd: 'STATUS', response_to: command.cmd, request_id: request.request_id });
      expect(session.routes.size).toBe(0);
      recreated.receiver_uid = 'new-logical-c1'; recreated.confirmed.baseband.frequency_hz = 7.11e6;
      recreated.state = 'stopped'; recreated.confirmed.enabled = false;
      session.snapshot.receivers = [recreated]; session.notify();
      expect(session.route(recreated.receiver_uid).enabled).toBe(false);
      recreated.state = 'running'; recreated.confirmed.enabled = true; session.notify();
      expect(session.route(recreated.receiver_uid).enabled).toBe(true);
      expect(session.masterGain).toBe(0.35); expect(session.masterMuted).toBe(true);
      expect(setNativeAudioRoute).toHaveBeenCalledWith('new-logical-c1', 'nfm', true, 0.7, 0, false);
      session.monitor(recreated.receiver_uid, { enabled: false }); session.notify();
      expect(session.route(recreated.receiver_uid).enabled).toBe(false);
    } finally { setWS(null); }
  });

  it.each(['ERROR', 'DISCONNECTED'])('cancels a pending main tune on %s before an unrelated receiver appears', kind => {
    const receiver = clone(session.snapshot.receivers[0]); receiver.receiver_uid = 'new-logical-c1'; receiver.confirmed.baseband.frequency_hz = 7.11e6;
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket; setWS(socket);
    try {
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true });
      const request = JSON.parse(String(vi.mocked(socket.send).mock.calls[0][0]));
      publishCommandReply({ cmd: kind, response_to: request.cmd, request_id: request.request_id });
      session.snapshot.receivers = [receiver]; session.notify();
      expect(session.route(receiver.receiver_uid).enabled).toBe(false);
    } finally { setWS(null); }
  });

  it('waits for the exact main tune acknowledgment when the receiver snapshot arrives first', () => {
    const receiver = clone(session.snapshot.receivers[0]); receiver.receiver_uid = 'new-logical-c1'; receiver.confirmed.baseband.frequency_hz = 7.11e6;
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket; setWS(socket);
    try {
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true });
      const request = JSON.parse(String(vi.mocked(socket.send).mock.calls[0][0]));
      session.snapshot.receivers = [receiver]; session.notify();
      expect(session.route(receiver.receiver_uid).enabled).toBe(false);
      publishCommandReply({ cmd: 'STATUS', response_to: request.cmd, request_id: 'older-request' });
      expect(session.route(receiver.receiver_uid).enabled).toBe(false);
      publishCommandReply({ cmd: 'STATUS', response_to: request.cmd, request_id: request.request_id });
      expect(session.route(receiver.receiver_uid).enabled).toBe(true);
    } finally { setWS(null); }
  });

  it('preserves an explicit monitor-off choice made before the main acknowledgment', () => {
    const receiver = clone(session.snapshot.receivers[0]); receiver.receiver_uid = 'new-logical-c1'; receiver.confirmed.baseband.frequency_hz = 7.11e6;
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket; setWS(socket);
    try {
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true });
      const request = JSON.parse(String(vi.mocked(socket.send).mock.calls[0][0]));
      session.snapshot.receivers = [receiver]; session.notify(); session.monitor(receiver.receiver_uid, { enabled: false });
      publishCommandReply({ cmd: 'STATUS', response_to: request.cmd, request_id: request.request_id });
      expect(session.route(receiver.receiver_uid).enabled).toBe(false);
    } finally { setWS(null); }
  });

  it('follows the latest tune while a recreated receiver awaits its first acknowledgment', () => {
    const receiver = clone(session.snapshot.receivers[0]); receiver.receiver_uid = 'new-logical-c1'; receiver.confirmed.baseband.frequency_hz = 7.11e6;
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket; setWS(socket);
    try {
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true });
      session.snapshot.receivers = [receiver]; session.notify();
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.12e6, enabled: true });
      const requests = vi.mocked(socket.send).mock.calls.map(([message]) => JSON.parse(String(message)));
      publishCommandReply({ cmd: 'STATUS', response_to: requests[0].cmd, request_id: requests[0].request_id });
      expect(session.route(receiver.receiver_uid).enabled).toBe(false);
      receiver.confirmed.baseband.frequency_hz = 7.12e6; session.notify();
      publishCommandReply({ cmd: 'STATUS', response_to: requests[1].cmd, request_id: requests[1].request_id });
      expect(session.route(receiver.receiver_uid).enabled).toBe(true);
    } finally { setWS(null); }
  });

  it('does not attach a pending main tune to another frequency or a restarted service', () => {
    const recreated = clone(session.snapshot.receivers[0]);
    session.snapshot = { ...clone(session.snapshot), receivers: [] }; session.notify();
    const socket = { readyState: WebSocket.OPEN, send: vi.fn() } as unknown as WebSocket;
    setWS(socket);
    try {
      send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.11e6, enabled: true });
      recreated.receiver_uid = 'new-logical-c1';
      session.snapshot.receivers = [recreated]; session.notify();
      expect(session.route(recreated.receiver_uid).enabled).toBe(false);
      session.snapshot.service_id = 'new-service'; recreated.confirmed.baseband.frequency_hz = 7.11e6;
      session.notify(); expect(session.route(recreated.receiver_uid).enabled).toBe(false);
    } finally { setWS(null); }
  });

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
  it.each(['disabled receiver', 'removed processor'])('drains the accepted tail when the directory reports a %s before its socket end', change => {
    const old = Worker.instances[0], neighbor = Worker.instances[1];
    session.snapshot = clone(session.snapshot);
    const receiver = session.snapshot.receivers[0];
    if (change === 'disabled receiver') {
      receiver.confirmed.enabled = false; receiver.state = 'stopped'; receiver.stream = null; receiver.clock = null;
    } else receiver.confirmed.processors = [];
    receiver.processing_state = 'disabled';
    receiver.processing_runtime = { state: 'disabled', processing_generation: '2', oldest_input_wait_ns: 0,
      input_held_bytes: 0, input_held_media_ns: 0, fault: null };
    session.notify();
    expect(old.terminate).not.toHaveBeenCalled(); expect(neighbor.terminate).not.toHaveBeenCalled();
    old.result();
    expect(streams.results('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
    expect(old.postMessage).toHaveBeenCalledWith({ type: 'ack' });
    old.onmessage!({ data: { type: 'ended', reason: 'receiver stopped' } });
    expect(old.terminate).toHaveBeenCalledOnce(); expect(Worker.instances).toHaveLength(2);
    session.notify(); old.result();
    expect(streams.results('fixture-c1').entries.filter(entry => entry.kind === 'text')).toHaveLength(1);
    expect(Worker.instances).toHaveLength(2);
  });
});
