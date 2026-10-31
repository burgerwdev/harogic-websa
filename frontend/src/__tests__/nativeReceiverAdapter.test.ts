import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NativeReceiverAdapter, type ReceivedIq } from '../core/nativeReceiverAdapter';
import { clone, type ServiceSnapshot } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';

class Socket {
  static instances: Socket[] = [];
  binaryType = ''; onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null; onclose: (() => void) | null = null;
  close = vi.fn(() => this.onclose?.());
  constructor(readonly url: URL) { Socket.instances.push(this); }
  emit(data: unknown): void { this.onmessage?.({ data: typeof data === 'object' && !(data instanceof ArrayBuffer) ? JSON.stringify(data) : data }); }
}
const response = (value: unknown) => ({ ok: true, json: async () => clone(value) });
const impact = { accepted: true, faults: [], capture_change: false, affected_receivers: [], affected_tracks: [], candidate_center_hz: 0, estimated_cf32_bytes_per_second: 0 };
let adapter: NativeReceiverAdapter, snapshot: ServiceSnapshot;
const fetchMock = vi.fn();
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); Socket.instances = [];
  vi.stubGlobal('WebSocket', Socket); vi.stubGlobal('fetch', fetchMock); fetchMock.mockReset();
  const fixture = new FixtureReceiverAdapter(); snapshot = fixture.snapshot(); fixture.dispose();
  snapshot.transport = 'native'; snapshot.service_id = 'boot-A'; snapshot.revision = '10';
  snapshot.capabilities.commands = ['create', 'configure', 'retire', 'output'];
  fetchMock.mockResolvedValue(response(snapshot)); adapter = await NativeReceiverAdapter.connect(); fetchMock.mockClear();
});
afterEach(() => { adapter.dispose(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); document.body.replaceChildren(); });
const uid = 'fixture-c1';
const output = () => adapter.dispatch({ type: 'output', request_id: 'subscribe', receiver_uid: uid, enabled: true });
function start(socket: Socket): void {
  socket.emit({ type: 'stream_start', receiver_uid: uid, stream: { source_id: 'primary', channel_id: '7', epoch: '9' },
    clock: { input_index: '9007199254740997', output_index: '100', fraction_num: '0', fraction_den: '1',
      input_rate_hz: 62500000, actual_rate_hz: 62500000 / 10368, delay_seconds: 0.012, utc_reliable: false } });
}
function block(magic = 'IQBF'): ArrayBuffer {
  const data = new ArrayBuffer(72), view = new DataView(data);
  [...magic].forEach((c, i) => view.setUint8(i, c.charCodeAt(0)));
  view.setUint32(4, 2, true); view.setUint32(8, 5, true); view.setUint32(12, 2, true);
  view.setFloat64(16, 62500000 / 10368, true); view.setUint32(32, 7, true); view.setUint32(36, 9, true);
  view.setBigUint64(40, 9007199254740997n + 20736n, true);
  new Float32Array(data, 56).set([0.5, -0.25, 0.125, 0.75]); return data;
}
describe('native receiver control and shared IQ', () => {
  it('uses service-confirmed replies and ignores a late lower revision', async () => {
    const config = clone(snapshot.receivers[0].confirmed); config.name = 'Requested';
    fetchMock.mockImplementation(async (_url, options) => {
      const request = JSON.parse(options.body);
      if (String(_url).endsWith('/preflight')) return response(impact);
      const next = clone(snapshot); next.revision = '12'; next.receivers[0].confirmed.name = 'Confirmed';
      return response({ request_id: request.request_id, snapshot: next, fault: null });
    });
    const reply = await adapter.dispatch({ type: 'configure', request_id: 'edit', receiver_uid: uid, expected_revision: '1', config });
    expect(reply.request_id).toBe('edit'); expect(adapter.snapshot().receivers[0].confirmed.name).toBe('Confirmed');
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); expect(adapter.snapshot().revision).toBe('12');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).service_id).toBe('boot-A');
  });
  it('preserves confirmed state on admission rejection and rejects mismatched response IDs', async () => {
    fetchMock.mockResolvedValueOnce(response({ ...impact, accepted: false, faults: [{ code: 'RF_RANGE' }] }));
    const command = { type: 'retire' as const, request_id: 'retire', receiver_uid: uid };
    expect((await adapter.dispatch(command)).fault?.code).toBe('RF_RANGE'); expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(adapter.snapshot()).toEqual(snapshot);
    fetchMock.mockResolvedValueOnce(response(impact)).mockResolvedValueOnce(response({ request_id: 'other', snapshot: { ...snapshot, revision: '99' }, fault: null }));
    await expect(adapter.dispatch(command)).rejects.toThrow('request_id mismatch'); expect(adapter.snapshot().revision).toBe('10');
  });
  it('cannot restore a retired service with a delayed command reply', async () => {
    let finish!: (value: unknown) => void;
    fetchMock.mockResolvedValueOnce(response(impact)).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    const pending = adapter.dispatch({ type: 'retire', request_id: 'old', receiver_uid: uid });
    await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
    const oldRequest = JSON.parse(fetchMock.mock.calls[1][1].body);
    fetchMock.mockResolvedValue(response({ ...snapshot, service_id: 'boot-B', revision: '1' })); await adapter.refresh();
    finish(response({ request_id: oldRequest.request_id, snapshot, fault: null }));
    await expect(pending).rejects.toThrow('restarted'); expect(adapter.snapshot().service_id).toBe('boot-B');
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); expect(adapter.snapshot().service_id).toBe('boot-B');
  });
  it('shares one subscription, retains precise clocks and delivers actual CF32 samples', async () => {
    const received: ReceivedIq[] = []; adapter.onIq(value => received.push(value));
    await output(); await output(); expect(Socket.instances).toHaveLength(1); expect(adapter.snapshot().outputs[0].state).toBe('starting');
    const socket = Socket.instances[0]; start(socket); socket.emit(block());
    expect(adapter.snapshot().outputs[0]).toMatchObject({ state: 'active', samples: '2', bytes: '72', actual_rate_hz: 62500000 / 10368 });
    expect(received[0].clock).toMatchObject({ input_index: '9007199254761733', output_index: '102', fraction_den: '1', delay_seconds: 0.012 });
    expect([...received[0].iq]).toEqual([0.5, -0.25, 0.125, 0.75]);
    socket.emit({ type: 'stream_boundary', next: null, reason: 'stopped', drained: true }); socket.close();
    expect(adapter.snapshot().outputs[0].state).toBe('ended'); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('isolates a throwing observer and rejects corrupted IQ without altering reception', async () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {}), receive = vi.fn();
    adapter.onIq(() => { throw new Error('observer'); }); adapter.onIq(receive);
    await output(); const socket = Socket.instances[0]; start(socket); socket.emit(block());
    expect(receive).toHaveBeenCalledOnce(); expect(log).toHaveBeenCalledOnce(); expect(adapter.snapshot().outputs[0].state).toBe('active');
    socket.emit(block('AUD2')); expect(adapter.snapshot().outputs[0].state).toBe('failed');
    expect(adapter.snapshot().receivers).toEqual(snapshot.receivers); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('reports a disconnect without a drained boundary as failure; disposing sends no stop', async () => {
    await output(); start(Socket.instances[0]); Socket.instances[0].close();
    expect(adapter.snapshot().outputs[0]).toMatchObject({ state: 'failed', fault: { code: 'DISCONNECTED' } });
    await output(); adapter.dispose(); expect(Socket.instances[1].close).toHaveBeenCalled(); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('preserves an unfocused draft and preview across polling; closing views keeps IQ alive', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const form = overview.querySelector<HTMLFormElement>('[data-config]')!, input = form.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '7123456'; input.dispatchEvent(new Event('input', { bubbles: true })); input.blur();
    await adapter.refresh(); expect(overview.querySelector<HTMLInputElement>('[name=frequency]')!.value).toBe('7123456');
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await adapter.refresh(); expect(overview.querySelector('[data-preview]')!.textContent).toContain('7123456');
    expect(overview.querySelector<HTMLButtonElement>('[data-action=rec-iq]')!.disabled).toBe(true);
    await output(); start(Socket.instances[0]); overview.querySelector<HTMLButtonElement>('[data-close]')!.click(); ui.recall(); ui.dispose(); session.dispose();
    expect(Socket.instances[0].close).not.toHaveBeenCalled(); expect(fetchMock.mock.calls.every(call => !call[1]?.method)).toBe(true);
  });
});
