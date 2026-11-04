import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { clone, type ReceiverConfig, type ServiceSnapshot } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { ReceiverModeMemory } from '../ui/receiverModeMemory';
import { WorkbenchSession } from '../ui/workbenchSession';

const key = ['websa', 'workbench', 'modes', 'v1'].join('-');
let snapshot: ServiceSnapshot;
beforeEach(() => {
  sessionStorage.clear();
  const adapter = new FixtureReceiverAdapter(); snapshot = adapter.snapshot(); adapter.dispose();
  snapshot.transport = 'native'; snapshot.service_id = 'service-a';
  snapshot.capabilities.mode_descriptors = ['am', 'usb'].map(id => ({ id, label: id, kind: 'analog', provider: 'native', available: true,
    unavailableReason: null, minimumRateHz: 48000, defaultBandwidthHz: 6000, sideband: 'symmetric', audioCompanion: null, outputs: ['audio'] }));
  snapshot.receivers[0].confirmed.processors[0].mode = 'am';
  Object.assign(snapshot.receivers[0].confirmed.processors[0], { bandwidth_hz: 8500, pcm_gain: 0.3, agc: false, squelch_dbm: -90,
    nr: true, nr_algorithm: 'dfn', nr_strength: 0.25, nr_atten_db: 14 });
});
afterEach(() => { vi.restoreAllMocks(); sessionStorage.clear(); });

it('remembers confirmed values per channel and mode across tab reload, without frequency, identity or task state', () => {
  const memory = new ReceiverModeMemory(); memory.observe(snapshot);
  const expected = memory.get('fixture-c1', 'am');
  expect(expected).toMatchObject({ nr: true, nr_algorithm: 'dfn', nr_strength: 0.25, nr_atten_db: 14 });
  snapshot.receivers[0].confirmed.processors[0].mode = 'usb'; memory.observe(snapshot);
  const restored = new ReceiverModeMemory(); restored.observe(snapshot);
  expect(restored.get('fixture-c1', 'am')).toEqual(expected);
  expect(restored.get('fixture-c2', 'am')).toBeUndefined();
  const encoded = sessionStorage.getItem(key)!;
  for (const excluded of ['frequency_hz', 'enabled', 'name', 'color', 'tags', 'recordings', 'stream', 'revision']) expect(encoded).not.toContain(`"${excluded}"`);
  expected!.bandwidth_hz = 9999;
  expect(restored.get('fixture-c1', 'am')?.bandwidth_hz).toBe(8500);
  // Current server confirmation wins over saved state, including an absent AGC's effective true default.
  delete snapshot.receivers[0].confirmed.processors[0].agc;
  restored.observe(snapshot); expect(restored.get('fixture-c1', 'usb')?.agc).toBe(true);
  const write = vi.spyOn(Storage.prototype, 'setItem'); restored.observe(snapshot); expect(write).not.toHaveBeenCalled();
});

it('forgets retired receivers and old service generations, even when the UID is reused', () => {
  const memory = new ReceiverModeMemory(); memory.observe(snapshot);
  const row = snapshot.receivers.shift()!; memory.observe(snapshot);
  expect(memory.get(row.receiver_uid, 'am')).toBeUndefined();
  row.confirmed.processors[0].mode = 'usb'; snapshot.receivers.push(row); memory.observe(snapshot);
  expect(memory.get(row.receiver_uid, 'am')).toBeUndefined();
  row.confirmed.processors[0].mode = 'am'; memory.observe(snapshot);
  snapshot.service_id = 'service-b'; row.confirmed.processors[0].mode = 'usb'; memory.observe(snapshot);
  expect(memory.get(row.receiver_uid, 'am')).toBeUndefined();
  const restored = new ReceiverModeMemory(); restored.observe(snapshot);
  expect(restored.get(row.receiver_uid, 'am')).toBeUndefined();
});

it('rejects corrupt, oversized and invalid stored values; blocked storage still permits in-memory recall', () => {
  for (const encoded of ['invalid json', ' '.repeat(256 * 1024 + 1), JSON.stringify({ service_id: snapshot.service_id,
    receivers: [['fixture-c1', [['usb', { passband_hz: -1 }], ['unknown', {}], null]]] })]) {
    sessionStorage.setItem(key, encoded);
    const memory = new ReceiverModeMemory(); memory.observe(snapshot);
    expect(memory.get('fixture-c1', 'am')?.bandwidth_hz).toBe(8500);
    expect(memory.get('fixture-c1', 'usb')).toBeUndefined(); expect(memory.get('fixture-c1', 'unknown')).toBeUndefined();
  }
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked'); });
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('full'); });
  const memory = new ReceiverModeMemory(); memory.observe(snapshot);
  expect(memory.get('fixture-c1', 'am')?.bandwidth_hz).toBe(8500);
});

it('learns only adapter confirmations, retaining confirmed settings after rejected edits and stale replies', async () => {
  let accept!: (s: ServiceSnapshot) => void;
  const dispatch = vi.fn(async (command: { request_id: string; config?: ReceiverConfig }) => ({ request_id: command.request_id, snapshot: clone(snapshot),
    fault: { scope: 'request' as const, code: 'REJECTED', message: 'rejected', retryable: true } }));
  const adapter = { snapshot: () => clone(snapshot), subscribe: (fn: typeof accept) => { accept = fn; return () => {}; }, dispatch, preflight: vi.fn() };
  const session = new WorkbenchSession(adapter);
  const draft = clone(snapshot.receivers[0].confirmed); draft.processors[0].bandwidth_hz = 9900;
  await session.configure('fixture-c1', draft);
  expect(session.modeMemory.get('fixture-c1', 'am')?.bandwidth_hz).toBe(8500);
  const next = clone(snapshot); next.revision = String(BigInt(snapshot.revision) + 1n); next.receivers[0].confirmed.processors[0].mode = 'usb'; accept(next);
  accept(snapshot); expect(session.snapshot.revision).toBe(next.revision);
  accept({ ...snapshot, service_id: 'service-b' });
  expect(session.modeMemory.get('fixture-c1', 'usb')).toBeUndefined();
  session.dispose();
});
